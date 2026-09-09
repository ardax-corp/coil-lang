//! Typeclass and instance registry for userland generics.
    //!
//! Stores the shapes of typeclasses (`Num<T>`, `Eq<T>`, …) and
//! registered implementations (`impl Num<int>`, `impl Num<float>`, …).
//! The [`Checker`](super::infer::Checker) owns one `Generics` value and
//! delegates type-class resolution to it.

use super::kind::Kind;
use super::subst::Subst;
use super::ty::{Ty, TyVarId};
use super::unify::unify_with;
use super::virtual_modules::{GC_MODULE, PRELUDE_MATH_MODULE, PRELUDE_MODULE, PRELUDE_OPS_MODULE};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// Legacy alias — builtin enums live under [`PRELUDE_MODULE`].
pub const BUILTIN_MODULE: &str = PRELUDE_MODULE;

//  Public data types

/// One method slot in a typeclass declaration.
#[derive(Debug, Clone)]
pub struct TypeClassMethodDef {
    /// Short method name (e.g. `"add"`).
    pub name: String,
    /// `true` when the class body contains a default implementation
    /// (a `Function` with a non-empty body).
    pub has_default: bool,
}

/// The shape of a typeclass: its name, type parameters, and methods.
    ///
/// E.g. `trait Num<T> { fn add(T a, T b) -> T; fn sub(…) -> T; }`
/// is stored as:
/// ```text
/// TypeClassDef { name: "Num", type_params: ["T"],
///     methods: [TypeClassMethodDef { name: "add", has_default: false }, …] }
/// ```
    ///
/// Superclasses (Phase 5): `trait Ordered<T: Equal>` stores
/// `superclasses: ["Equal"]`. Dictionary layout is flattened — subclass
/// methods first, then each superclass’s methods in declaration order
/// (transitively).
    ///
/// Associated types: `trait Collect<C> { type Elem<A>; … }`
/// stores structured declarations so generic associated types retain
/// their own binders and kind.
#[derive(Debug, Clone)]
pub struct TypeClassDef {
    pub name: String,
    /// Module path that declared this typeclass. Builtins use
    /// [`BUILTIN_MODULE`].
    pub defined_module: String,
    /// Type-parameter names in declaration order, e.g. `["T"]`.
    pub type_params: Vec<String>,
    /// Kinds of each type parameter (parallel to `type_params`).
    /// Empty means every parameter has kind `*`.
    pub param_kinds: Vec<Kind>,
    /// Direct superclass class names from single-parameter bounds
    /// (`trait Ord<T: Eq>` → `["Eq"]`). Empty for multi-param classes
    /// and classes without bounds.
    pub superclasses: Vec<String>,
    /// Associated type declarations in source order.
    pub assoc_types: Vec<AssocTypeDecl>,
    pub methods: Vec<TypeClassMethodDef>,
}

/// One associated type declaration in a typeclass body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssocTypeDecl {
    pub name: String,
    /// GAT parameter names in declaration order.
    pub params: Vec<String>,
    /// Kinds of each GAT parameter (parallel to `params`).
    pub param_kinds: Vec<Kind>,
    /// Full kind of the associated type constructor. Nullary associated
    /// types have kind `*`; `type Ref<T>;` has kind `* -> *`.
    pub kind: Kind,
}

impl AssocTypeDecl {
    pub fn new(name: impl Into<String>, params: Vec<String>, param_kinds: Vec<Kind>) -> Self {
        let kind = param_kinds
            .iter()
            .rev()
            .cloned()
            .fold(Kind::Type, |codomain, domain| Kind::arrow(domain, codomain));
        Self {
            name: name.into(),
            params,
            param_kinds,
            kind,
        }
    }
}

/// One associated type definition inside an instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssocTypeValue {
    pub params: Vec<String>,
    pub param_vars: Vec<TyVarId>,
    pub param_kinds: Vec<Kind>,
    pub ty: Ty,
}

impl TypeClassDef {
    /// Kind of parameter `i`, defaulting to `*`.
    pub fn kind_at(&self, i: usize) -> Kind {
        self.param_kinds.get(i).cloned().unwrap_or(Kind::Type)
    }

    /// True when parameter `i` is constructor-kinded.
    pub fn is_constructor_kind_at(&self, i: usize) -> bool {
        self.kind_at(i).is_constructor_kind()
    }

    /// Constructor arity of parameter `i`, if it is constructor-kinded.
    pub fn constructor_arity_at(&self, i: usize) -> Option<usize> {
        let kind = self.kind_at(i);
        kind.is_constructor_kind().then(|| kind.arity())
    }

    pub fn assoc_type(&self, name: &str) -> Option<&AssocTypeDecl> {
        self.assoc_types.iter().find(|decl| decl.name == name)
    }

    /// True when `name` is a transitive superclass of this class.
    pub fn has_superclass(&self, name: &str, generics: &Generics) -> bool {
        let mut seen = HashSet::new();
        let mut stack: Vec<&str> = self.superclasses.iter().map(String::as_str).collect();
        while let Some(super_name) = stack.pop() {
            if !seen.insert(super_name.to_string()) {
                continue;
            }
            if super_name == name {
                return true;
            }
            if let Some(super_def) = generics.typeclass(super_name) {
                stack.extend(super_def.superclasses.iter().map(String::as_str));
            }
        }
        false
    }

    /// Flattened dictionary method list: this class’s methods, then each
    /// superclass’s methods in declaration order (DFS, cycle-safe).
    ///
    /// Returns `(owning_class_name, method_def)` pairs. Slot index in the
    /// runtime dict is the position in this list.
    pub fn flattened_methods<'a>(
        &'a self,
        generics: &'a Generics,
    ) -> Vec<(&'a str, &'a TypeClassMethodDef)> {
        let mut out = Vec::new();
        let mut visited = HashSet::new();
        Self::collect_flattened_methods(self, generics, &mut out, &mut visited);
        out
    }

    fn collect_flattened_methods<'a>(
        class: &'a TypeClassDef,
        generics: &'a Generics,
        out: &mut Vec<(&'a str, &'a TypeClassMethodDef)>,
        visited: &mut HashSet<String>,
    ) {
        if !visited.insert(class.name.clone()) {
            return;
        }
        for method in &class.methods {
            out.push((class.name.as_str(), method));
        }
        for super_name in &class.superclasses {
            if let Some(super_def) = generics.typeclass(super_name) {
                Self::collect_flattened_methods(super_def, generics, out, visited);
            }
        }
    }
}

/// One registered typeclass instance (concrete type → implementation mapping).
    ///
/// E.g. `impl Num<int> { fn add(…) { … } }` is stored as:
/// ```text
/// InstanceDef { class: "Num", args: [int()],
///     method_fqns: { "add" → "Num__int__add" } }
/// ```
    ///
/// Associated types: `impl Collect<Option<int>> { type Elem<A> = A; … }`
/// stores `assoc_tys: { "Elem" → AssocTypeValue { … } }`.
#[derive(Debug, Clone)]
pub struct InstanceDef {
    /// Class name this instance implements (e.g. `"Num"`).
    pub class: String,
    /// Module path that declared this instance. Builtins use
    /// [`BUILTIN_MODULE`].
    pub defined_module: String,
    /// Source span of the `impl` declaration inside `defined_module`.
    pub range: Range<usize>,
    /// Concrete type arguments (e.g. `[int()]` for `impl Num<int>`).
    pub args: Vec<Ty>,
    /// Method name → fully-qualified codegen name.
    pub method_fqns: HashMap<String, String>,
    /// Associated type name → RHS template with its own binders.
    pub assoc_tys: HashMap<String, AssocTypeValue>,
}

//  Generics registry

/// Registry of typeclasses and their instances, owned by [`Checker`].
#[derive(Debug, Default)]
pub struct Generics {
    /// All declared typeclasses.
    pub typeclasses: HashMap<String, TypeClassDef>,
    /// All registered instances (in declaration order).
    pub instances: Vec<InstanceDef>,
    /// Generic type constructors: enum/class/alias name → type param names.
    pub generic_type_ctors: HashMap<String, Vec<String>>,
    /// Nominal type ownership: enum/class/alias name → defining module path.
    pub nominal_type_modules: HashMap<String, String>,
    /// Generic function names (have at least one type param).
    pub generic_fns: std::collections::HashSet<String>,
}

impl Generics {
    pub fn new() -> Self {
        let mut g = Self::default();
        g.register_builtins();
        g
    }

    /// Register compiler-provided generic type constructors that are
    /// always in scope.
    pub fn register_builtin_type_ctors(&mut self) {
        self.generic_type_ctors
            .insert(common::BUILTIN_OPTION_ENUM.into(), vec!["T".into()]);
        self.register_nominal_type(common::BUILTIN_OPTION_ENUM, BUILTIN_MODULE);
        self.generic_type_ctors.insert(
            common::BUILTIN_RESULT_ENUM.into(),
            vec!["T".into(), "E".into()],
        );
        self.register_nominal_type(common::BUILTIN_RESULT_ENUM, BUILTIN_MODULE);
        // Lazy ranges (`a..b` / `a..=b`) — element type must satisfy `Ord`.
        self.generic_type_ctors
            .insert("Range".into(), vec!["T".into()]);
        self.register_nominal_type("Range", PRELUDE_MODULE);
        self.generic_type_ctors
            .insert("RangeInclusive".into(), vec!["T".into()]);
        self.register_nominal_type("RangeInclusive", PRELUDE_MODULE);
        self.register_nominal_type("ArrayIter", PRELUDE_MODULE);
        // Nominal matrix wrapper: `Matrix<Data>` where `Data` is nested
        // static rows (`[[T; N]; M]`). `*` is matmul (Mul), not zip.
        self.generic_type_ctors
            .insert(common::BUILTIN_MATRIX_TYPE.into(), vec!["T".into()]);
        self.register_nominal_type(common::BUILTIN_MATRIX_TYPE, PRELUDE_MATH_MODULE);
        self.generic_type_ctors
            .insert(common::BUILTIN_ROOT_TYPE.into(), vec!["T".into()]);
        self.register_nominal_type(common::BUILTIN_ROOT_TYPE, GC_MODULE);
        self.generic_type_ctors
            .insert(common::BUILTIN_WEAK_TYPE.into(), vec!["T".into()]);
        self.register_nominal_type(common::BUILTIN_WEAK_TYPE, GC_MODULE);
        self.generic_type_ctors
            .insert(common::BUILTIN_VEC_TYPE.into(), vec!["T".into()]);
        self.register_nominal_type(common::BUILTIN_VEC_TYPE, PRELUDE_MODULE);
    }

    pub fn register_nominal_type(&mut self, name: &str, module: &str) {
        self.nominal_type_modules
            .insert(name.to_string(), module.to_string());
    }

    pub fn nominal_type_module(&self, name: &str) -> Option<&str> {
        self.nominal_type_modules.get(name).map(String::as_str)
    }

    /// Look up a typeclass by name.
    pub fn typeclass(&self, name: &str) -> Option<&TypeClassDef> {
        self.typeclasses.get(name)
    }

    /// Find an instance for `class` applied to `args` (exact type match).
    pub fn find_instance(&self, class: &str, args: &[Ty]) -> Option<&InstanceDef> {
        self.instances.iter().find(|inst| {
            inst.class == class
                && inst.args.len() == args.len()
                && inst.args.iter().zip(args.iter()).all(|(a, b)| a == b)
        })
    }

    /// Find an instance whose args unify with `args` (e.g. `Option::Some(42)`'s
    /// Constructor type against `impl Collect<Option<int>>`). Does not mutate
    /// any global substitution.
    pub fn find_unifying_instances(&self, class: &str, args: &[Ty]) -> Vec<&InstanceDef> {
        self.instances
            .iter()
            .filter(|inst| Self::instances_unify(class, args, inst))
            .collect()
    }

    pub fn find_unifying_instance(&self, class: &str, args: &[Ty]) -> Option<&InstanceDef> {
        self.find_unifying_instances(class, args).into_iter().next()
    }

    /// Exact match, then unifying match (Constructor ↔ App/Sum).
    pub fn find_instance_relaxed(&self, class: &str, args: &[Ty]) -> Option<&InstanceDef> {
        self.find_instance(class, args)
            .or_else(|| self.find_unifying_instance(class, args))
    }

    /// True when an identical class + concrete-argument instance already exists.
    pub fn has_overlapping_instance(&self, class: &str, args: &[Ty]) -> bool {
        self.find_overlapping_instance(class, args).is_some()
    }

    pub fn find_overlapping_instance(&self, class: &str, args: &[Ty]) -> Option<&InstanceDef> {
        self.instances
            .iter()
            .find(|inst| Self::instances_unify(class, args, inst))
    }

    pub fn instances_unify(class: &str, args: &[Ty], inst: &InstanceDef) -> bool {
        if inst.class != class || inst.args.len() != args.len() {
            return false;
        }
        let mut local = Subst::empty();
        for (have, need) in inst.args.iter().zip(args.iter()) {
            match unify_with(&local, need, have) {
                Ok(s) => local = s,
                Err(_) => return false,
            }
        }
        true
    }

    /// Required class methods omitted by an instance.
    pub fn missing_required_methods(
        class_def: &TypeClassDef,
        method_fqns: &HashMap<String, String>,
    ) -> Vec<String> {
        class_def
            .methods
            .iter()
            .filter(|method| !method.has_default && !method_fqns.contains_key(&method.name))
            .map(|method| method.name.clone())
            .collect()
    }

    /// Methods provided by an instance that are not declared by its class.
    pub fn unknown_instance_methods<'a, I>(class_def: &TypeClassDef, method_names: I) -> Vec<String>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let known: HashSet<&str> = class_def
            .methods
            .iter()
            .map(|method| method.name.as_str())
            .collect();
        method_names
            .into_iter()
            .filter(|name| !known.contains(*name))
            .map(str::to_string)
            .collect()
    }

    /// Associated types required by the class but missing from the instance.
    pub fn missing_assoc_types(
        class_def: &TypeClassDef,
        assoc_tys: &HashMap<String, AssocTypeValue>,
    ) -> Vec<String> {
        class_def
            .assoc_types
            .iter()
            .filter(|decl| !assoc_tys.contains_key(decl.name.as_str()))
            .map(|decl| decl.name.clone())
            .collect()
    }

    /// Associated types provided by an instance that the class does not declare.
    pub fn unknown_assoc_types<'a, I>(class_def: &TypeClassDef, assoc_names: I) -> Vec<String>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let known: HashSet<&str> = class_def
            .assoc_types
            .iter()
            .map(|decl| decl.name.as_str())
            .collect();
        assoc_names
            .into_iter()
            .filter(|name| !known.contains(*name))
            .map(str::to_string)
            .collect()
    }

    /// FQN used for default typeclass methods. The class default lives outside
    /// any concrete instance, so its slot is keyed as `{Class}__default__{method}`.
    pub fn default_method_fqn(class: &str, method: &str) -> String {
        format!("{}__default__{}", class, method)
    }

    /// Populate omitted defaulted methods so every satisfiable class method has
    /// a dict slot FQN after instance checking.
    pub fn fill_default_method_fqns(
        class_def: &TypeClassDef,
        method_fqns: &mut HashMap<String, String>,
    ) {
        for method in &class_def.methods {
            if method.has_default {
                method_fqns
                    .entry(method.name.clone())
                    .or_insert_with(|| Self::default_method_fqn(&class_def.name, &method.name));
            }
        }
    }

    /// Build the compiler-generated FQN for a builtin instance method.
    ///
    /// Convention: `{Class}__{concrete_type_str}__{method}`
    /// e.g. `Add__int__add`, `Lt__float__lt`, `Eq__string__eq`.
    pub fn builtin_instance_fqn(class: &str, ty_str: &str, method: &str) -> String {
        format!("{}__{}__{}", class, ty_str, method)
    }

    /// Register the built-in typeclasses and their builtin instances.
    fn register_builtins(&mut self) {
        use super::ty::{INT, Ty, boolean, float, int, string, unit};

        self.register_builtin_type_ctors();

        // Individual arithmetic traits so a type can implement only the
        // operations it supports. `Num` is a convenience supertrait that
        // implies all four (see below).
        for (name, method) in [
            ("Add", "add"),
            ("Sub", "sub"),
            ("Mul", "mul"),
            ("Div", "div"),
        ] {
            self.typeclasses.insert(
                name.into(),
                TypeClassDef {
                    name: name.into(),
                    defined_module: PRELUDE_OPS_MODULE.into(),
                    type_params: vec!["T".into()],
                    param_kinds: vec![Kind::Type],
                    superclasses: vec![],
                    assoc_types: vec![],
                    methods: vec![TypeClassMethodDef {
                        name: method.into(),
                        has_default: false,
                    }],
                },
            );
        }

        // Convenience bundle: `T: Num` implies Add + Sub + Mul + Div via
        // the flattened superclass dictionary layout. Num itself has no
        // methods; call sites resolve operators through the op traits.
        self.typeclasses.insert(
            "Num".into(),
            TypeClassDef {
                name: "Num".into(),
                defined_module: PRELUDE_OPS_MODULE.into(),
                type_params: vec!["T".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec!["Add".into(), "Sub".into(), "Mul".into(), "Div".into()],
                assoc_types: vec![],
                methods: vec![],
            },
        );

        // Individual ordering traits so a type can implement only the
        // comparisons it supports. `Ord` is a convenience supertrait that
        // implies all four (see below).
        for (name, method) in [("Lt", "lt"), ("Le", "le"), ("Gt", "gt"), ("Ge", "ge")] {
            self.typeclasses.insert(
                name.into(),
                TypeClassDef {
                    name: name.into(),
                    defined_module: PRELUDE_OPS_MODULE.into(),
                    type_params: vec!["T".into()],
                    param_kinds: vec![Kind::Type],
                    superclasses: vec![],
                    assoc_types: vec![],
                    methods: vec![TypeClassMethodDef {
                        name: method.into(),
                        has_default: false,
                    }],
                },
            );
        }

        // Convenience bundle: `T: Ord` implies Lt + Le + Gt + Ge via the
        // flattened superclass dictionary layout. Ord itself has no methods.
        // Builtin Ord does not declare Eq as a superclass. User-defined
        // classes use `trait Ordered<T: Equal>` for that path.
        self.typeclasses.insert(
            "Ord".into(),
            TypeClassDef {
                name: "Ord".into(),
                defined_module: PRELUDE_OPS_MODULE.into(),
                type_params: vec!["T".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec!["Lt".into(), "Le".into(), "Gt".into(), "Ge".into()],
                assoc_types: vec![],
                methods: vec![],
            },
        );

        self.typeclasses.insert(
            "Eq".into(),
            TypeClassDef {
                name: "Eq".into(),
                defined_module: PRELUDE_OPS_MODULE.into(),
                type_params: vec!["T".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec![],
                assoc_types: vec![],
                methods: vec![
                    TypeClassMethodDef {
                        name: "eq".into(),
                        has_default: false,
                    },
                    TypeClassMethodDef {
                        name: "ne".into(),
                        has_default: false,
                    },
                ],
            },
        );

        self.typeclasses.insert(
            "Show".into(),
            TypeClassDef {
                name: "Show".into(),
                defined_module: PRELUDE_OPS_MODULE.into(),
                type_params: vec!["T".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec![],
                assoc_types: vec![],
                methods: vec![TypeClassMethodDef {
                    name: "show".into(),
                    has_default: false,
                }],
            },
        );

        // `len(x)` resolves through this trait for custom types; arrays,
        // tuples, and dicts keep a structural fast path in the checker.
        self.typeclasses.insert(
            "Length".into(),
            TypeClassDef {
                name: "Length".into(),
                defined_module: PRELUDE_OPS_MODULE.into(),
                type_params: vec!["T".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec![],
                assoc_types: vec![],
                methods: vec![TypeClassMethodDef {
                    name: "len".into(),
                    has_default: false,
                }],
            },
        );

        // (#[derive] targets; see compiler/src/attrs.rs)
        self.typeclasses.insert(
            "Default".into(),
            TypeClassDef {
                name: "Default".into(),
                defined_module: PRELUDE_MODULE.into(),
                type_params: vec!["T".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec![],
                assoc_types: vec![],
                methods: vec![TypeClassMethodDef {
                    name: "default".into(),
                    has_default: false,
                }],
            },
        );
        self.typeclasses.insert(
            "Hash".into(),
            TypeClassDef {
                name: "Hash".into(),
                defined_module: PRELUDE_MODULE.into(),
                type_params: vec!["T".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec![],
                assoc_types: vec![],
                methods: vec![TypeClassMethodDef {
                    name: "hash".into(),
                    has_default: false,
                }],
            },
        );
        self.typeclasses.insert(
            "Serialize".into(),
            TypeClassDef {
                name: "Serialize".into(),
                defined_module: PRELUDE_MODULE.into(),
                type_params: vec!["T".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec![],
                assoc_types: vec![],
                methods: vec![TypeClassMethodDef {
                    name: "serialize".into(),
                    has_default: false,
                }],
            },
        );
        self.typeclasses.insert(
            "Deserialize".into(),
            TypeClassDef {
                name: "Deserialize".into(),
                defined_module: PRELUDE_MODULE.into(),
                type_params: vec!["T".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec![],
                assoc_types: vec![],
                methods: vec![TypeClassMethodDef {
                    name: "deserialize".into(),
                    has_default: false,
                }],
            },
        );
        self.typeclasses.insert(
            "Send".into(),
            TypeClassDef {
                name: "Send".into(),
                defined_module: PRELUDE_MODULE.into(),
                type_params: vec!["T".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec![],
                assoc_types: vec![],
                methods: vec![],
            },
        );
        self.typeclasses.insert(
            "String".into(),
            TypeClassDef {
                name: "String".into(),
                defined_module: PRELUDE_MODULE.into(),
                type_params: vec!["T".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec![],
                assoc_types: vec![],
                methods: vec![TypeClassMethodDef {
                    name: "to_string".into(),
                    has_default: false,
                }],
            },
        );
        self.typeclasses.insert(
            "Sensitive".into(),
            TypeClassDef {
                name: "Sensitive".into(),
                defined_module: PRELUDE_MODULE.into(),
                type_params: vec!["T".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec![],
                assoc_types: vec![],
                methods: vec![],
            },
        );

        // Multi-param conversion trait (auto-imported via prelude::ops).
        // `impl Into<T> for Self` → instance args [Self, T]; method
        // `into` has type `Self → T`. No builtin instances — users write
        // the impls they need. Prefer `x.into()` with an expected type
        // (`let y: T = x.into();`) over a reverse `From` encoding.
        self.typeclasses.insert(
            "Into".into(),
            TypeClassDef {
                name: "Into".into(),
                defined_module: PRELUDE_OPS_MODULE.into(),
                type_params: vec!["Self".into(), "T".into()],
                param_kinds: vec![Kind::Type, Kind::Type],
                superclasses: vec![],
                assoc_types: vec![],
                methods: vec![TypeClassMethodDef {
                    name: "into".into(),
                    has_default: false,
                }],
            },
        );

        self.typeclasses.insert(
            "Read".into(),
            TypeClassDef {
                name: "Read".into(),
                defined_module: "io".into(),
                type_params: vec!["S".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec![],
                assoc_types: vec![],
                methods: vec![TypeClassMethodDef {
                    name: "read".into(),
                    has_default: false,
                }],
            },
        );
        self.typeclasses.insert(
            "Write".into(),
            TypeClassDef {
                name: "Write".into(),
                defined_module: "io".into(),
                type_params: vec!["S".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec![],
                assoc_types: vec![],
                methods: vec![TypeClassMethodDef {
                    name: "write".into(),
                    has_default: false,
                }],
            },
        );

        // Helper: build method_fqns map for an instance.
        let make_fqns = |class: &str, ty_str: &str, methods: &[&str]| -> HashMap<String, String> {
            methods
                .iter()
                .map(|m| (m.to_string(), Self::builtin_instance_fqn(class, ty_str, m)))
                .collect()
        };

        for ty in [int(), float()] {
            let ty_str = match &ty {
                Ty::Con(n) if n == INT => "int",
                _ => "float",
            };
            for (class, method) in [
                ("Add", "add"),
                ("Sub", "sub"),
                ("Mul", "mul"),
                ("Div", "div"),
                ("Lt", "lt"),
                ("Le", "le"),
                ("Gt", "gt"),
                ("Ge", "ge"),
            ] {
                self.instances.push(InstanceDef {
                    class: class.into(),
                    defined_module: PRELUDE_OPS_MODULE.into(),
                    range: 0..0,
                    args: vec![ty.clone()],
                    method_fqns: make_fqns(class, ty_str, &[method]),
                    assoc_tys: HashMap::new(),
                });
            }
            // Num / Ord instances: no own methods; dict emission pulls
            // superclass slots via the flattened layout.
            for class in ["Num", "Ord"] {
                self.instances.push(InstanceDef {
                    class: class.into(),
                    defined_module: PRELUDE_OPS_MODULE.into(),
                    range: 0..0,
                    args: vec![ty.clone()],
                    method_fqns: HashMap::new(),
                    assoc_tys: HashMap::new(),
                });
            }
        }

        self.instances.push(InstanceDef {
            class: "Eq".into(),
            defined_module: PRELUDE_OPS_MODULE.into(),
            range: 0..0,
            args: vec![int()],
            method_fqns: make_fqns("Eq", "int", &["eq", "ne"]),
            assoc_tys: HashMap::new(),
        });
        self.instances.push(InstanceDef {
            class: "Show".into(),
            defined_module: PRELUDE_OPS_MODULE.into(),
            range: 0..0,
            args: vec![int()],
            method_fqns: make_fqns("Show", "int", &["show"]),
            assoc_tys: HashMap::new(),
        });
        self.instances.push(InstanceDef {
            class: "Eq".into(),
            defined_module: PRELUDE_OPS_MODULE.into(),
            range: 0..0,
            args: vec![float()],
            method_fqns: make_fqns("Eq", "float", &["eq", "ne"]),
            assoc_tys: HashMap::new(),
        });
        self.instances.push(InstanceDef {
            class: "Show".into(),
            defined_module: PRELUDE_OPS_MODULE.into(),
            range: 0..0,
            args: vec![float()],
            method_fqns: make_fqns("Show", "float", &["show"]),
            assoc_tys: HashMap::new(),
        });

        self.instances.push(InstanceDef {
            class: "Eq".into(),
            defined_module: PRELUDE_OPS_MODULE.into(),
            range: 0..0,
            args: vec![string()],
            method_fqns: make_fqns("Eq", "string", &["eq", "ne"]),
            assoc_tys: HashMap::new(),
        });
        self.instances.push(InstanceDef {
            class: "Show".into(),
            defined_module: PRELUDE_OPS_MODULE.into(),
            range: 0..0,
            args: vec![string()],
            method_fqns: make_fqns("Show", "string", &["show"]),
            assoc_tys: HashMap::new(),
        });
        self.instances.push(InstanceDef {
            class: "Length".into(),
            defined_module: PRELUDE_OPS_MODULE.into(),
            range: 0..0,
            args: vec![string()],
            method_fqns: make_fqns("Length", "string", &["len"]),
            assoc_tys: HashMap::new(),
        });

        self.instances.push(InstanceDef {
            class: "Eq".into(),
            defined_module: PRELUDE_OPS_MODULE.into(),
            range: 0..0,
            args: vec![boolean()],
            method_fqns: make_fqns("Eq", "bool", &["eq", "ne"]),
            assoc_tys: HashMap::new(),
        });
        self.instances.push(InstanceDef {
            class: "Show".into(),
            defined_module: PRELUDE_OPS_MODULE.into(),
            range: 0..0,
            args: vec![boolean()],
            method_fqns: make_fqns("Show", "bool", &["show"]),
            assoc_tys: HashMap::new(),
        });

        self.instances.push(InstanceDef {
            class: "Show".into(),
            defined_module: PRELUDE_OPS_MODULE.into(),
            range: 0..0,
            args: vec![unit()],
            method_fqns: make_fqns("Show", "unit", &["show"]),
            assoc_tys: HashMap::new(),
        });

        for (ty, ty_str) in [
            (int(), "int"),
            (float(), "float"),
            (string(), "string"),
            (boolean(), "bool"),
            (unit(), "unit"),
            (super::ty::byte(), "byte"),
        ] {
            self.instances.push(InstanceDef {
                class: "Hash".into(),
                defined_module: PRELUDE_MODULE.into(),
                range: 0..0,
                args: vec![ty],
                method_fqns: make_fqns("Hash", ty_str, &["hash"]),
                assoc_tys: HashMap::new(),
            });
        }

        self.instances.push(InstanceDef {
            class: "Eq".into(),
            defined_module: PRELUDE_OPS_MODULE.into(),
            range: 0..0,
            args: vec![super::ty::byte()],
            method_fqns: make_fqns("Eq", "byte", &["eq", "ne"]),
            assoc_tys: HashMap::new(),
        });
        self.instances.push(InstanceDef {
            class: "Show".into(),
            defined_module: PRELUDE_OPS_MODULE.into(),
            range: 0..0,
            args: vec![super::ty::byte()],
            method_fqns: make_fqns("Show", "byte", &["show"]),
            assoc_tys: HashMap::new(),
        });
        for (class, method) in [("Lt", "lt"), ("Le", "le"), ("Gt", "gt"), ("Ge", "ge")] {
            self.instances.push(InstanceDef {
                class: class.into(),
                defined_module: PRELUDE_OPS_MODULE.into(),
                range: 0..0,
                args: vec![super::ty::byte()],
                method_fqns: make_fqns(class, "byte", &[method]),
                assoc_tys: HashMap::new(),
            });
        }
        self.instances.push(InstanceDef {
            class: "Ord".into(),
            defined_module: PRELUDE_OPS_MODULE.into(),
            range: 0..0,
            args: vec![super::ty::byte()],
            method_fqns: HashMap::new(),
            assoc_tys: HashMap::new(),
        });

        let into_pairs: [(&str, Ty, &str, Ty); 6] = [
            ("int", int(), "float", float()),
            ("float", float(), "int", int()),
            ("int", int(), "byte", super::ty::byte()),
            ("byte", super::ty::byte(), "int", int()),
            ("int", int(), "bool", boolean()),
            ("bool", boolean(), "int", int()),
        ];
        for (from_s, from_ty, to_s, to_ty) in into_pairs {
            let mut method_fqns = HashMap::new();
            method_fqns.insert(
                "into".to_string(),
                format!("Into__{}__to_{}__into", from_s, to_s),
            );
            self.instances.push(InstanceDef {
                class: "Into".into(),
                defined_module: PRELUDE_OPS_MODULE.into(),
                range: 0..0,
                args: vec![from_ty.clone(), to_ty.clone()],
                method_fqns,
                assoc_tys: HashMap::new(),
            });
        }

        self.instances.push(InstanceDef {
            class: "Read".into(),
            defined_module: "io".into(),
            range: 0..0,
            args: vec![super::ty::stream_ty()],
            method_fqns: make_fqns("Read", "Stream", &["read"]),
            assoc_tys: HashMap::new(),
        });
        self.instances.push(InstanceDef {
            class: "Write".into(),
            defined_module: "io".into(),
            range: 0..0,
            args: vec![super::ty::stream_ty()],
            method_fqns: make_fqns("Write", "Stream", &["write"]),
            assoc_tys: HashMap::new(),
        });

        // Carrier as first type param (same shape as Collect). Builtin
        // arrays/tuples/dicts/coroutines are synthesised at for-in sites
        // without a ground InstanceDef per T; users write ordinary `impl`.
        self.typeclasses.insert(
            "Iterator".into(),
            TypeClassDef {
                name: "Iterator".into(),
                defined_module: PRELUDE_MODULE.into(),
                type_params: vec!["I".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec![],
                assoc_types: vec![AssocTypeDecl::new("Item", vec![], vec![])],
                methods: vec![TypeClassMethodDef {
                    name: "next".into(),
                    has_default: false,
                }],
            },
        );
        self.typeclasses.insert(
            "IntoIterator".into(),
            TypeClassDef {
                name: "IntoIterator".into(),
                defined_module: PRELUDE_MODULE.into(),
                type_params: vec!["T".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec![],
                assoc_types: vec![
                    AssocTypeDecl::new("Item", vec![], vec![]),
                    AssocTypeDecl::new("IntoIter", vec![], vec![]),
                ],
                methods: vec![TypeClassMethodDef {
                    name: "into_iter".into(),
                    has_default: false,
                }],
            },
        );
        // Opaque prelude iterator state type (arrays materialise as this
        // conceptually; codegen fast-paths never allocate a real class).
        self.register_nominal_type("ArrayIter", PRELUDE_MODULE);
    }

    /// Whether `class` is satisfied for `ty` (builtin or registered instance).
    pub fn has_instance(&self, class: &str, ty: &Ty) -> bool {
        self.find_instance(class, std::slice::from_ref(ty))
            .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typechecking::ty::{
        TyVarId, boolean, byte, float, int, option_app_ty, string, unit,
    };

    fn method(name: &str, has_default: bool) -> TypeClassMethodDef {
        TypeClassMethodDef {
            name: name.to_string(),
            has_default,
        }
    }

    fn class_def() -> TypeClassDef {
        TypeClassDef {
            name: "Num".to_string(),
            defined_module: "test".to_string(),
            type_params: vec!["T".to_string()],
            param_kinds: vec![Kind::Type],
            superclasses: vec![],
            assoc_types: vec![],
            methods: vec![method("add", false), method("zero", true)],
        }
    }

    #[test]
    fn flattened_methods_subclass_then_superclass() {
        let mut generics = Generics::new();
        generics.typeclasses.insert(
            "Equal".into(),
            TypeClassDef {
                name: "Equal".into(),
                defined_module: "test".into(),
                type_params: vec!["T".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec![],
                assoc_types: vec![],
                methods: vec![method("eq_val", false)],
            },
        );
        generics.typeclasses.insert(
            "Ordered".into(),
            TypeClassDef {
                name: "Ordered".into(),
                defined_module: "test".into(),
                type_params: vec!["T".into()],
                param_kinds: vec![Kind::Type],
                superclasses: vec!["Equal".into()],
                assoc_types: vec![],
                methods: vec![method("lt_val", false)],
            },
        );
        let ordered = generics.typeclass("Ordered").unwrap();
        let flat: Vec<_> = ordered
            .flattened_methods(&generics)
            .into_iter()
            .map(|(c, m)| (c.to_string(), m.name.clone()))
            .collect();
        assert_eq!(
            flat,
            vec![
                ("Ordered".into(), "lt_val".into()),
                ("Equal".into(), "eq_val".into()),
            ]
        );
        assert!(ordered.has_superclass("Equal", &generics));
        assert!(!ordered.has_superclass("Num", &generics));
    }

    #[test]
    fn has_overlapping_instance_detects_exact_class_and_args() {
        let mut generics = Generics::new();
        generics.instances.clear();
        generics.instances.push(InstanceDef {
            class: "Num".to_string(),
            defined_module: "test".to_string(),
            range: 0..0,
            args: vec![int()],
            method_fqns: HashMap::new(),
            assoc_tys: HashMap::new(),
        });

        assert!(generics.has_overlapping_instance("Num", &[int()]));
        assert!(!generics.has_overlapping_instance("Num", &[float()]));
        assert!(!generics.has_overlapping_instance("Show", &[int()]));
    }

    #[test]
    fn has_overlapping_instance_detects_unifying_args() {
        let mut generics = Generics::new();
        generics.instances.clear();
        generics.instances.push(InstanceDef {
            class: "Collect".to_string(),
            defined_module: "test".to_string(),
            range: 0..0,
            args: vec![option_app_ty(int())],
            method_fqns: HashMap::new(),
            assoc_tys: HashMap::new(),
        });

        assert!(
            generics.has_overlapping_instance("Collect", &[option_app_ty(Ty::Var(TyVarId(999)))])
        );
    }

    #[test]
    fn missing_required_methods_ignores_default_methods() {
        let mut method_fqns = HashMap::new();
        method_fqns.insert("zero".to_string(), "Num__int__zero".to_string());

        assert_eq!(
            Generics::missing_required_methods(&class_def(), &method_fqns),
            vec!["add".to_string()]
        );
    }

    #[test]
    fn unknown_instance_methods_reports_methods_not_in_class() {
        let unknown = Generics::unknown_instance_methods(&class_def(), ["add", "foo"].into_iter());

        assert_eq!(unknown, vec!["foo".to_string()]);
    }

    #[test]
    fn fill_default_method_fqns_registers_omitted_defaults() {
        let mut method_fqns = HashMap::new();
        method_fqns.insert("add".to_string(), "Num__int__add".to_string());

        Generics::fill_default_method_fqns(&class_def(), &mut method_fqns);

        assert_eq!(method_fqns.get("add").unwrap(), "Num__int__add");
        assert_eq!(method_fqns.get("zero").unwrap(), "Num__default__zero");
    }

    #[test]
    fn derivable_prelude_traits_registered() {
        let g = Generics::new();
        for name in [
            "Default",
            "Hash",
            "Serialize",
            "Deserialize",
            "Send",
            "String",
            "Sensitive",
        ] {
            assert!(
                g.typeclass(name).is_some(),
                "missing prelude typeclass `{name}`"
            );
        }
        assert_eq!(g.typeclass("Default").unwrap().methods[0].name, "default");
        assert_eq!(g.typeclass("String").unwrap().methods[0].name, "to_string");
        assert!(g.typeclass("Send").unwrap().methods.is_empty());
    }

    #[test]
    fn hash_builtin_instances_cover_primitives() {
        let g = Generics::new();
        for ty in [int(), float(), string(), boolean(), unit(), byte()] {
            assert!(
                g.find_instance("Hash", &[ty.clone()]).is_some(),
                "missing Hash instance for {ty:?}"
            );
        }
    }

    #[test]
    fn find_instance_matches_multi_arg_class() {
        let mut generics = Generics::new();
        generics.instances.push(InstanceDef {
            class: "Convert".to_string(),
            defined_module: "test".to_string(),
            range: 0..0,
            args: vec![int(), int()],
            method_fqns: HashMap::from([(
                "cast".to_string(),
                "Convert__int_int__cast".to_string(),
            )]),
            assoc_tys: HashMap::new(),
        });

        assert!(generics.find_instance("Convert", &[int(), int()]).is_some());
        assert!(
            generics
                .find_instance("Convert", &[int(), float()])
                .is_none()
        );
        assert!(generics.find_instance("Convert", &[int()]).is_none());
    }
}
