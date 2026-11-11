//! Typeclass and instance registry for userland generics.
//!
//! Stores the shapes of typeclasses (`Num<T>`, `Eq<T>`, …) and
//! registered implementations (`impl Num for int`, `impl Num for float`, …).
//! The [`Checker`](super::infer::Checker) owns one `Generics` value and
//! delegates type-class resolution to it.

use super::kind::Kind;
use super::subst::Subst;
use super::ty::{Constraint, Ty, TyVarId};
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
    /// `true` for `static fn` methods: no `Self`-typed receiver; called as
    /// `Owner::name(..)` or `T::name(..)` under a bound.
    pub is_static: bool,
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
/// E.g. `impl Num for int { fn add(…) { … } }` is stored as:
/// ```text
/// InstanceDef { class: "Num", args: [int()],
///     method_fqns: { "add" → "Num__int__add" } }
/// ```
///
/// Associated types: `impl Collect for Option<int> { type Elem<A> = A; … }`
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
    /// Bounds on a generic head's parameters (`impl Show for Box<T: Show>`
    /// → `[Show<'a>]`, over the variables in `args`). Empty for concrete
    /// and unbounded instances.
    pub context: Vec<Constraint>,
}

/// A builtin trait the registry starts with.
struct BuiltinTrait {
    name: &'static str,
    module: &'static str,
    params: &'static [&'static str],
    superclasses: &'static [&'static str],
    methods: &'static [&'static str],
    /// Its methods are called as `T::m()` (`Default::default`).
    static_methods: bool,
    assoc_types: &'static [&'static str],
    /// A prelude trait ([`crate::typechecking::infer::Checker::is_builtin_class`]):
    /// the core prelude may redeclare it with default bodies, and a
    /// `use prelude… as Alias` frees its name. `Into` and the io traits are not.
    prelude: bool,
}

const fn op_trait(name: &'static str, methods: &'static [&'static str]) -> BuiltinTrait {
    BuiltinTrait {
        name,
        module: PRELUDE_OPS_MODULE,
        params: &["T"],
        superclasses: &[],
        methods,
        static_methods: false,
        assoc_types: &[],
        prelude: true,
    }
}

const fn bundle(name: &'static str, superclasses: &'static [&'static str]) -> BuiltinTrait {
    BuiltinTrait { superclasses, ..op_trait(name, &[]) }
}

const fn prelude_trait(name: &'static str, methods: &'static [&'static str]) -> BuiltinTrait {
    BuiltinTrait { module: PRELUDE_MODULE, ..op_trait(name, methods) }
}

/// The builtin traits. Each arithmetic, bitwise and ordering operator has
/// its own trait so a type can implement only the operations it supports;
/// `Num`, `Integral` and `Ord` are method-less bundles over them (`Ord`
/// takes `Eq` too, so `==` in a `T: Ord` body uses `T`'s `Eq`, #1716).
const BUILTIN_TRAITS: [BuiltinTrait; 32] = [
    op_trait("Add", &["add"]),
    op_trait("Sub", &["sub"]),
    op_trait("Mul", &["mul"]),
    op_trait("Div", &["div"]),
    op_trait("Neg", &["neg"]),
    op_trait("Rem", &["rem"]),
    op_trait("Pow", &["pow"]),
    op_trait("Shl", &["shl"]),
    op_trait("Shr", &["shr"]),
    op_trait("BitAnd", &["bitand"]),
    op_trait("BitOr", &["bitor"]),
    op_trait("BitXor", &["bitxor"]),
    op_trait("BitNot", &["bitnot"]),
    bundle("Num", &["Add", "Sub", "Mul", "Div", "Neg", "Rem", "Pow"]),
    // `float` is Num but not Integral.
    bundle("Integral", &["Num", "Shl", "Shr", "BitAnd", "BitOr", "BitXor", "BitNot"]),
    op_trait("Lt", &["lt"]),
    op_trait("Le", &["le"]),
    op_trait("Gt", &["gt"]),
    op_trait("Ge", &["ge"]),
    bundle("Ord", &["Lt", "Le", "Gt", "Ge", "Eq"]),
    op_trait("Eq", &["eq", "ne"]),
    op_trait("Show", &["show"]),
    op_trait("Length", &["len"]),
    // `#[derive]` targets (compiler/src/attrs.rs); `static fn default() -> T`.
    BuiltinTrait { static_methods: true, ..prelude_trait("Default", &["default"]) },
    prelude_trait("Hash", &["hash"]),
    prelude_trait("Send", &[]),
    prelude_trait("Sensitive", &[]),
    // Multi-param conversion: `impl Into<T> for Self` → instance args
    // `[Self, T]`. Prefer `x.into()` with an expected type over a reverse
    // `From` encoding.
    BuiltinTrait { params: &["Self", "T"], prelude: false, ..op_trait("Into", &["into"]) },
    BuiltinTrait { module: "io", params: &["S"], prelude: false, ..op_trait("Read", &["read"]) },
    BuiltinTrait { module: "io", params: &["S"], prelude: false, ..op_trait("Write", &["write"]) },
    // Carrier as first type param (same shape as Collect). Builtin arrays,
    // tuples, dicts and coroutines are synthesised at for-in sites without
    // a ground instance per T; users write ordinary `impl`.
    BuiltinTrait { params: &["I"], assoc_types: &["Item"], ..prelude_trait("Iterator", &["next"]) },
    BuiltinTrait { assoc_types: &["Item", "IntoIter"], ..prelude_trait("IntoIterator", &["into_iter"]) },
];

/// Whether `class` is a builtin prelude trait ([`BuiltinTrait::prelude`]).
pub fn is_prelude_trait(class: &str) -> bool {
    BUILTIN_TRAITS.iter().any(|t| t.prelude && t.name == class)
}

//  Generics registry

/// Registry of typeclasses and their instances, owned by [`Checker`].
#[derive(Clone, Debug, Default)]
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
        // `thread` handles carry their value type (#1905). The `thread`
        // module exports the names; a bare `Thread` infers its `T`.
        for handle in crate::typechecking::ty::THREAD_HANDLES {
            self.generic_type_ctors.insert(handle.into(), vec!["T".into()]);
        }
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
    /// The arguments a `Show`, `Eq` or `Hash` goal is looked up by: a
    /// `Matrix<D>` in them is its data `D`, at any depth (#2466). A matrix
    /// is its data at run time and shows, compares and hashes as it does;
    /// no instance of these traits has a `Matrix` head (both are builtin).
    pub fn matrix_data_args<'a>(class: &str, args: &'a [Ty]) -> std::borrow::Cow<'a, [Ty]> {
        use crate::typechecking::aggregate_arith::{erase_matrix_ty, is_matrix_ty};
        if matches!(class, "Show" | "Eq" | "Hash") && args.iter().any(|ty| ty.any(is_matrix_ty)) {
            return std::borrow::Cow::Owned(args.iter().map(erase_matrix_ty).collect());
        }
        std::borrow::Cow::Borrowed(args)
    }

    pub fn find_instance(&self, class: &str, args: &[Ty]) -> Option<&InstanceDef> {
        let args = &*Self::matrix_data_args(class, args);
        self.instances.iter().find(|inst| {
            inst.class == class
                && inst.args.len() == args.len()
                && inst.args.iter().zip(args.iter()).all(|(a, b)| a == b)
        })
    }

    /// Find an instance whose args unify with `args` (e.g. `Option::Some(42)`'s
    /// Constructor type against `impl Collect for Option<int>`). Does not mutate
    /// any global substitution.
    pub fn find_unifying_instances(&self, class: &str, args: &[Ty]) -> Vec<&InstanceDef> {
        let args = &*Self::matrix_data_args(class, args);
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
    /// Builtin enums with a builtin `Show` instance, with their variants in
    /// tag order (`Show__IoError__show` returns the variant's name).
    pub const BUILTIN_SHOW_ENUMS: [(&'static str, &'static [&'static str]); 8] = [
        (common::BUILTIN_IO_ERROR_ENUM, common::BUILTIN_IO_ERROR_VARIANTS),
        (common::BUILTIN_TTY_ERROR_ENUM, common::BUILTIN_TTY_ERROR_VARIANTS),
        (common::BUILTIN_STRING_ERROR_ENUM, common::BUILTIN_STRING_ERROR_VARIANTS),
        (common::BUILTIN_THREAD_ERROR_ENUM, common::BUILTIN_THREAD_ERROR_VARIANTS),
        (common::BUILTIN_CHANNEL_ERROR_ENUM, common::BUILTIN_CHANNEL_ERROR_VARIANTS),
        (common::BUILTIN_LOCK_ERROR_ENUM, common::BUILTIN_LOCK_ERROR_VARIANTS),
        (common::BUILTIN_ENV_ERROR_ENUM, common::BUILTIN_ENV_ERROR_VARIANTS),
        (common::BUILTIN_FFI_ERROR_KIND_ENUM, common::BUILTIN_FFI_ERROR_KIND_VARIANTS),
    ];

    pub fn builtin_instance_fqn(class: &str, ty_str: &str, method: &str) -> String {
        format!("{}__{}__{}", class, ty_str, method)
    }

    /// The operator trait and method a binary arithmetic or bitwise operator
    /// dispatches through on a bound type parameter or a user type (`%` is
    /// `Rem::rem`, `<<` is `Shl::shl`).
    pub fn operator_trait(op: &str) -> Option<(&'static str, &'static str)> {
        Some(match op {
            "+" => ("Add", "add"),
            "-" => ("Sub", "sub"),
            "*" => ("Mul", "mul"),
            "/" => ("Div", "div"),
            "%" => ("Rem", "rem"),
            "**" => ("Pow", "pow"),
            "<<" => ("Shl", "shl"),
            ">>" => ("Shr", "shr"),
            "&" => ("BitAnd", "bitand"),
            "|" => ("BitOr", "bitor"),
            "^" => ("BitXor", "bitxor"),
            _ => return None,
        })
    }

    /// True for the integer-only operator traits (`<<`, `>>`, `&`, `|`,
    /// `^`, `~`): `float` has no instance.
    pub fn is_bitwise_trait(class: &str) -> bool {
        matches!(class, "Shl" | "Shr" | "BitAnd" | "BitOr" | "BitXor" | "BitNot")
    }

    /// Register the built-in typeclasses and their builtin instances.
    fn register_builtins(&mut self) {
        use super::ty::{BUILTIN_SCALARS, BuiltinScalar, STREAM};

        self.register_builtin_type_ctors();

        for t in &BUILTIN_TRAITS {
            self.typeclasses.insert(
                t.name.into(),
                TypeClassDef {
                    name: t.name.into(),
                    defined_module: t.module.into(),
                    type_params: t.params.iter().map(|p| (*p).into()).collect(),
                    param_kinds: vec![Kind::Type; t.params.len()],
                    superclasses: t.superclasses.iter().map(|c| (*c).into()).collect(),
                    assoc_types: t.assoc_types.iter().map(|a| AssocTypeDecl::new(*a, vec![], vec![])).collect(),
                    methods: t
                        .methods
                        .iter()
                        .map(|m| TypeClassMethodDef {
                            name: (*m).into(),
                            has_default: false,
                            is_static: t.static_methods,
                        })
                        .collect(),
                },
            );
        }

        // Each scalar's instances; Num / Ord / Integral have no own methods
        // (dict emission pulls superclass slots via the flattened layout).
        for scalar in &BUILTIN_SCALARS {
            for class in scalar.traits {
                self.push_builtin_instance(class, Ty::Con(scalar.name.into()), scalar.name);
            }
        }
        // The builtin error enums render as their variant name (`NotFound`).
        for name in Self::BUILTIN_SHOW_ENUMS.iter().map(|(name, _)| *name) {
            self.push_builtin_instance("Show", Ty::Con(name.into()), name);
        }
        // `Into` follows the primitive casts (`int` ↔ `float`, …).
        for scalar in &BUILTIN_SCALARS {
            for to in scalar.casts_to {
                let to_ty = BuiltinScalar::named(to).map(|s| Ty::Con(s.name.into())).expect("a scalar");
                self.instances.push(InstanceDef {
                    class: "Into".into(),
                    defined_module: PRELUDE_OPS_MODULE.into(),
                    range: 0..0,
                    args: vec![Ty::Con(scalar.name.into()), to_ty],
                    method_fqns: HashMap::from([("into".to_string(), format!("Into__{}__to_{}__into", scalar.name, to))]),
                    assoc_tys: HashMap::new(),
                    context: Vec::new(),
                });
            }
        }
        for class in ["Read", "Write"] {
            self.push_builtin_instance(class, Ty::Con(STREAM.into()), STREAM);
        }

        // Opaque prelude iterator state type (arrays materialise as this
        // conceptually; codegen fast-paths never allocate a real class).
        self.register_nominal_type("ArrayIter", PRELUDE_MODULE);
    }

    /// Register builtin `class`'s instance for `ty` (spelled `ty_str` in
    /// its methods' names), in the class's module.
    fn push_builtin_instance(&mut self, class: &str, ty: Ty, ty_str: &str) {
        let def = &self.typeclasses[class];
        let method_fqns =
            def.methods.iter().map(|m| (m.name.clone(), Self::builtin_instance_fqn(class, ty_str, &m.name))).collect();
        self.instances.push(InstanceDef {
            class: class.into(),
            defined_module: def.defined_module.clone(),
            range: 0..0,
            args: vec![ty],
            method_fqns,
            assoc_tys: HashMap::new(),
            context: Vec::new(),
        });
    }

    /// Register the structural instance of `class` for the ground tuple,
    /// record, array, `Option` or `Result` `ty` (#1451, #1479): `Show` as `%v` shows it, `Eq`
    /// as `==` compares it, `Hash` combining its items', and a tuple's
    /// `Lt` / `Le` / `Gt` / `Ge` / `Ord` ordering it lexicographically (#1721), and a
    /// homogeneous aggregate's `Add` .. `Pow`, `Neg` and `Num` as its
    /// element-wise operators compute them (#2513). The checker adds
    /// one the first time a goal asks for it
    /// (`Checker::ensure_structural_instance`); codegen emits its thunks
    /// ([`Self::structural_instances`]).
    pub fn register_structural_instance(&mut self, class: &str, ty: Ty) {
        let methods: &[&str] = match class {
            "Show" => &["show"],
            "Eq" => &["eq", "ne"],
            "Hash" => &["hash"],
            "Length" => &["len"],
            // A tuple's lexicographic order (#1721); `Ord` has no methods.
            "Lt" => &["lt"],
            "Le" => &["le"],
            "Gt" => &["gt"],
            "Ge" => &["ge"],
            "Ord" => &[],
            // An aggregate's element-wise arithmetic (#2513); `Num` has no
            // methods.
            "Add" => &["add"],
            "Sub" => &["sub"],
            "Mul" => &["mul"],
            "Div" => &["div"],
            "Rem" => &["rem"],
            "Pow" => &["pow"],
            "Neg" => &["neg"],
            "Num" => &[],
            _ => return,
        };
        let ty_str = ty.to_string();
        self.instances.push(InstanceDef {
            class: class.into(),
            defined_module: PRELUDE_OPS_MODULE.into(),
            range: 0..0,
            args: vec![ty],
            method_fqns: methods.iter().map(|m| (m.to_string(), Self::builtin_instance_fqn(class, &ty_str, m))).collect(),
            assoc_tys: HashMap::new(),
            context: Vec::new(),
        });
    }

    /// Add the ground structural instances [`Self::register_structural_instance`]
    /// registered in `other` that this one lacks. They are program-wide, as
    /// the prelude's: a later module's goal (`Eq<Option<int>>`) registers
    /// one that a clone compiled in an earlier module's context needs (#2354).
    pub fn adopt_structural_instances(&mut self, other: &Generics) {
        for inst in &other.instances {
            if Self::is_structural_ground(inst) && self.find_instance(&inst.class, &inst.args).is_none()
            {
                self.instances.push(inst.clone());
            }
        }
    }

    /// The method of `class`'s structural instance for tuples of `arity`
    /// with open items (#1479): `Show__(_, _)__show`.
    pub fn structural_tuple_fqn(class: &str, arity: usize, method: &str) -> String {
        let items = vec!["_"; arity].join(", ");
        let shape = if arity == 1 { format!("({items},)") } else { format!("({items})") };
        Self::builtin_instance_fqn(class, &shape, method)
    }

    /// The method of `class`'s structural instance for the open shape
    /// `head` ([`Self::register_structural_open`]), its variables spelled
    /// `_`: `Show__(_, _)__show`, `Eq__Option<_>__eq`, `Show__[_]__show`.
    pub fn structural_open_fqn(class: &str, head: &Ty, method: &str) -> String {
        match head {
            Ty::Tuple(items) => Self::structural_tuple_fqn(class, items.len(), method),
            _ => Self::builtin_instance_fqn(class, &head.subst_vars(&|_| Some(Ty::Con("_".into()))).to_string(), method),
        }
    }

    /// The structural instances with open items: `Show` of a tuple,
    /// record, `Option`, `Result`, `Vec` or array, `Eq` of those, `Hash` of
    /// a tuple, record or array, a tuple's `Lt` .. `Ge` / `Ord` (#1479,
    /// #2554, #2631). Head `head` is the shape over fresh variables
    /// (`(a, b)`, `{ x: a, y: b }` in name order, `Option<a>`, `[a]`,
    /// which a fixed array matches), context `context<a>`, .. (one
    /// dictionary per variable, in the head's order; `context` is `class`,
    /// or `Ord` for an order, whose items compare with `<` and `<=`), so
    /// a goal on the shape at a type parameter (`Show<Option<T>>`) meets
    /// the class inside a generic body with the frame's `Show<T>`. Its
    /// thunks read each variable's dictionary from their own. A no-op when
    /// one is registered.
    pub fn register_structural_open(&mut self, class: &str, head: Ty, context: &str) {
        let methods: &[&str] = match class {
            "Show" => &["show"],
            "Eq" => &["eq", "ne"],
            "Hash" => &["hash"],
            "Lt" => &["lt"],
            "Le" => &["le"],
            "Gt" => &["gt"],
            "Ge" => &["ge"],
            "Ord" => &[],
            _ => return,
        };
        if self.has_structural_open(class, &head) {
            return;
        }
        let mut vars = Vec::new();
        head.collect_vars(&mut vars);
        let context = vars
            .iter()
            .map(|&v| Constraint {
                class: context.into(),
                args: vec![Ty::Var(v)],
            })
            .collect();
        let method_fqns = methods.iter().map(|m| (m.to_string(), Self::structural_open_fqn(class, &head, m))).collect();
        self.instances.push(InstanceDef {
            class: class.into(),
            defined_module: PRELUDE_OPS_MODULE.into(),
            range: 0..0,
            args: vec![head],
            method_fqns,
            assoc_tys: HashMap::new(),
            context,
        });
    }

    /// Whether `inst` is a structural instance for a shape with open items
    /// ([`Self::register_structural_open`]).
    pub fn is_structural_open(inst: &InstanceDef) -> bool {
        inst.range == (0..0) && inst.defined_module == PRELUDE_OPS_MODULE && !inst.context.is_empty() && inst.args.len() == 1
    }

    /// Whether `inst` is a ground structural instance
    /// ([`Self::register_structural_instance`] at a type with no variable).
    pub fn is_structural_ground(inst: &InstanceDef) -> bool {
        inst.range == (0..0)
            && inst.defined_module == PRELUDE_OPS_MODULE
            && inst.context.is_empty()
            && inst.args.iter().all(|a| super::ty::ftv_ty(a).is_empty())
    }

    /// Whether `inst` is a structural instance for a shape with open
    /// elements (`Length<Vec<a>>`, `Length<(a, b)>`, #1542): registered by
    /// [`Self::register_structural_instance`] over fresh variables, with
    /// no context. A goal another instance meets does not take it.
    pub fn is_structural_shape(inst: &InstanceDef) -> bool {
        inst.range == (0..0)
            && inst.defined_module == PRELUDE_OPS_MODULE
            && inst.context.is_empty()
            && inst.args.iter().any(|a| !super::ty::ftv_ty(a).is_empty())
    }

    /// `(class, head, method, method fqn)` of each method of each instance
    /// [`Self::register_structural_open`] registered.
    pub fn structural_opens(&self) -> impl Iterator<Item = (&str, &Ty, &str, &str)> {
        self.instances
            .iter()
            .filter(|inst| Self::is_structural_open(inst))
            .flat_map(|inst| inst.method_fqns.iter().map(move |(m, f)| (inst.class.as_str(), &inst.args[0], m.as_str(), f.as_str())))
    }

    /// Whether `class` has the structural instance for the shape of `head`
    /// ([`Self::register_structural_open`]; its variables or holes match
    /// any).
    pub fn has_structural_open(&self, class: &str, head: &Ty) -> bool {
        let key = Self::structural_open_fqn(class, head, "");
        self.instances
            .iter()
            .any(|inst| inst.class == class && Self::is_structural_open(inst) && Self::structural_open_fqn(class, &inst.args[0], "") == key)
    }

    /// Whether `fqn` is a method of an instance
    /// [`Self::register_structural_open`] registered.
    pub fn is_structural_open_fqn(&self, fqn: &str) -> bool {
        self.structural_opens().any(|(.., f)| f == fqn)
    }

    /// The `(class, type)` of each instance
    /// [`Self::register_structural_instance`] registered.
    pub fn structural_instances(&self) -> impl Iterator<Item = (&str, &Ty)> {
        self.instances
            .iter()
            .filter(|inst| inst.range == (0..0) && inst.defined_module == PRELUDE_OPS_MODULE && inst.context.is_empty())
            .filter_map(|inst| match inst.args.as_slice() {
                [ty @ (Ty::Tuple(_) | Ty::Record { .. } | Ty::Array { .. })] => Some((inst.class.as_str(), ty)),
                // `Length` of a `Vec` (#1536), its `Eq` (#1629), `Show` (#1848)
                // and `Neg` (#2564).
                [ty] if matches!(inst.class.as_str(), "Length" | "Eq" | "Show" | "Neg") && super::ty::vec_element_ty(ty).is_some() => Some((inst.class.as_str(), ty)),
                [ty @ Ty::App(head, _)]
                    if matches!(head.as_ref(), Ty::Con(n) if n == common::BUILTIN_OPTION_ENUM || n == common::BUILTIN_RESULT_ENUM) =>
                {
                    Some((inst.class.as_str(), ty))
                }
                _ => None,
            })
    }

    /// Whether `class` is satisfied for `ty` (builtin or registered instance).
    pub fn has_instance(&self, class: &str, ty: &Ty) -> bool {
        self.find_instance(class, std::slice::from_ref(ty))
            .is_some()
    }

    /// A generic instance (`Show for Box<T>`, head with type variables) whose
    /// head unifies with `args`: the fallback after an exact
    /// [`Self::find_instance`] miss. Concrete instances are never returned.
    pub fn find_generic_instance(&self, class: &str, args: &[Ty]) -> Option<&InstanceDef> {
        let args = &*Self::matrix_data_args(class, args);
        self.instances.iter().find(|inst| {
            inst.args.iter().any(Ty::has_var) && Self::instances_unify(class, args, inst)
        })
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::typechecking::ty::{
        TyVarId, boolean, byte, float, int, option_ty, string, unit,
    };

    fn method(name: &str, has_default: bool) -> TypeClassMethodDef {
        TypeClassMethodDef {
            name: name.to_string(),
            has_default,
            is_static: false,
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
            context: Vec::new(),
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
            args: vec![option_ty(int())],
            method_fqns: HashMap::new(),
            assoc_tys: HashMap::new(),
            context: Vec::new(),
        });

        assert!(
            generics.has_overlapping_instance("Collect", &[option_ty(Ty::Var(TyVarId(999)))])
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
        let unknown = Generics::unknown_instance_methods(&class_def(), ["add", "foo"]);

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
            "Send",
            "Sensitive",
        ] {
            assert!(
                g.typeclass(name).is_some(),
                "missing prelude typeclass `{name}`"
            );
        }
        assert_eq!(g.typeclass("Default").unwrap().methods[0].name, "default");
        assert!(g.typeclass("String").is_none(), "`String` was folded into `Show`");
        assert!(g.typeclass("Send").unwrap().methods.is_empty());
    }

    #[test]
    fn reserved_type_names_are_the_builtin_type_ctors() {
        // A user type may not take a builtin ctor's name (#1654): the list
        // the checker rejects must track what is registered here.
        let mut g = Generics::default();
        g.register_builtin_type_ctors();
        let mut registered: Vec<&str> = g
            .nominal_type_modules
            .keys()
            .map(String::as_str)
            .filter(|n| !common::is_poly_builtin_enum(n))
            .collect();
        registered.sort_unstable();
        let mut reserved = common::BUILTIN_RESERVED_TYPE_NAMES.to_vec();
        reserved.sort_unstable();
        assert_eq!(registered, reserved);
    }

    #[test]
    fn hash_builtin_instances_cover_primitives() {
        let g = Generics::new();
        for ty in [int(), float(), string(), boolean(), unit(), byte()] {
            assert!(
                g.find_instance("Hash", std::slice::from_ref(&ty)).is_some(),
                "missing Hash instance for {ty:?}"
            );
        }
    }

    #[test]
    fn builtin_scalar_instances_follow_the_tables() {
        use crate::typechecking::ty::BUILTIN_SCALARS;
        let g = Generics::new();
        for scalar in &BUILTIN_SCALARS {
            let ty = [Ty::Con(scalar.name.into())];
            for class in scalar.traits {
                let inst = g.find_instance(class, &ty).unwrap_or_else(|| panic!("{class} for {}", scalar.name));
                assert_eq!(inst.method_fqns.len(), g.typeclass(class).unwrap().methods.len());
            }
            for to in scalar.casts_to {
                assert!(g.find_instance("Into", &[ty[0].clone(), Ty::Con((*to).into())]).is_some());
            }
        }
        assert_eq!(
            g.find_instance("Eq", &[int()]).unwrap().method_fqns["ne"],
            "Eq__int__ne",
        );
        assert!(g.find_instance("BitNot", &[byte()]).is_none());
        assert!(is_prelude_trait("Iterator") && !is_prelude_trait("Into") && !is_prelude_trait("Read"));
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
            context: Vec::new(),
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
