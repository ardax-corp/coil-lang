//! Monotypes ([`Ty`]), polytypes ([`Scheme`]), and type-variable ids.
//!
//! Type variables are minted by [`Checker`](super::infer::Checker) and valid
//! only for that checker's lifetime.

use std::collections::HashSet;

/// Identifier for a type variable.
///
/// IDs are minted by `Checker` and are only meaningful for the lifetime of
/// that `Checker`. They are ordered by minting time so debug output is
/// stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TyVarId(pub u32);

impl TyVarId {
    /// The underlying integer. Used by the pretty-printer.
    pub fn raw(self) -> u32 {
        self.0
    }
}

/// Monomorphic types.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    Var(TyVarId),
    Con(String),
    Fun(Box<Ty>, Box<Ty>),
    App(Box<Ty>, Vec<Ty>),
    Sum {
        name: String,
        variants: Vec<(String, EnumVariantPayloadTy)>,
    },
    Constructor {
        owner: Box<Ty>,
        tag: u32,
        arity: usize,
    },
    Tuple(Vec<Ty>),
    Array {
        element: Box<Ty>,
        length: ArrayLength,
    },
    Record {
        fields: Vec<(String, Ty)>,
    },
    /// `readonly T` — sealed against external mutation (typechecker only in v1).
    Readonly(Box<Ty>),
    Existential {
        class: String,
    },
    Forall {
        bounds: Vec<TyVarId>,
        constraints: Vec<Constraint>,
        body: Box<Ty>,
    },
    /// Bottom type for diverging control flow (`return`/`raise`/`panic`/proven
    /// infinite loops). Checker-only — never a runtime/ABI type.
    Never,
}

/// Array length: compile-time constant or runtime-known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArrayLength {
    Static(usize),
    Dynamic,
}

impl ArrayLength {
    pub fn is_static(&self) -> bool {
        matches!(self, ArrayLength::Static(_))
    }
}

/// Variant payload shape: unit, tuple, or record fields.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EnumVariantPayloadTy {
    Unit,
    Tuple(Vec<Ty>),
    Record(Vec<(String, Ty)>),
}

/// Runtime word for a scalar-backed enum case (`#[repr(int)]` / `Case = lit`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ScalarBacking {
    Int(i64),
    /// IEEE bits so backing values can be `Eq`/`Hash`.
    Float(u64),
    String(String),
    Bool(bool),
}

impl ScalarBacking {
    pub fn kind_name(&self) -> &'static str {
        match self {
            ScalarBacking::Int(_) => "int",
            ScalarBacking::Float(_) => "float",
            ScalarBacking::String(_) => "string",
            ScalarBacking::Bool(_) => "bool",
        }
    }

    pub fn ty(&self) -> Ty {
        match self {
            ScalarBacking::Int(_) => int(),
            ScalarBacking::Float(_) => float(),
            ScalarBacking::String(_) => string(),
            ScalarBacking::Bool(_) => boolean(),
        }
    }
}

impl EnumVariantPayloadTy {
    pub fn field_count(&self) -> usize {
        match self {
            EnumVariantPayloadTy::Unit => 0,
            EnumVariantPayloadTy::Tuple(tys) => tys.len(),
            EnumVariantPayloadTy::Record(fields) => fields.len(),
        }
    }

    pub fn field_types(&self) -> Vec<&Ty> {
        match self {
            EnumVariantPayloadTy::Unit => Vec::new(),
            EnumVariantPayloadTy::Tuple(tys) => tys.iter().collect(),
            EnumVariantPayloadTy::Record(fields) => fields.iter().map(|(_, ty)| ty).collect(),
        }
    }

    /// Field names and types in declaration order. Tuple variants use
    /// synthetic `"0"`, `"1"`, … names (codegen reordering only).
    pub fn field_pairs(&self) -> Vec<(String, Ty)> {
        match self {
            EnumVariantPayloadTy::Unit => Vec::new(),
            EnumVariantPayloadTy::Tuple(tys) => tys
                .iter()
                .enumerate()
                .map(|(i, ty)| (i.to_string(), ty.clone()))
                .collect(),
            EnumVariantPayloadTy::Record(fields) => fields.clone(),
        }
    }
}

impl EnumVariantPayloadTy {
    /// The payload's field types, in declaration order.
    pub fn types(&self) -> impl Iterator<Item = &Ty> {
        let (tuple, record): (&[Ty], &[(String, Ty)]) = match self {
            EnumVariantPayloadTy::Unit => (&[], &[]),
            EnumVariantPayloadTy::Tuple(tys) => (tys, &[]),
            EnumVariantPayloadTy::Record(fields) => (&[], fields),
        };
        tuple.iter().chain(record.iter().map(|(_, ty)| ty))
    }

    /// The payload with `f` applied to each field type; its shape and field
    /// names are kept.
    pub fn map_types(&self, mut f: impl FnMut(&Ty) -> Ty) -> EnumVariantPayloadTy {
        match self {
            EnumVariantPayloadTy::Unit => EnumVariantPayloadTy::Unit,
            EnumVariantPayloadTy::Tuple(tys) => EnumVariantPayloadTy::Tuple(tys.iter().map(f).collect()),
            EnumVariantPayloadTy::Record(fields) => {
                EnumVariantPayloadTy::Record(fields.iter().map(|(n, t)| (n.clone(), f(t))).collect())
            }
        }
    }
}

/// Traversal. Each walk over a `Ty` is written on these, so every shape
/// is visited: an enum's payloads (unit, tuple and record) and a `forall`'s
/// body and constraint arguments included. They are blind to binders: a
/// walk that must treat a `forall`'s bound variables apart (free variables,
/// substitution) matches [`Ty::Forall`] itself and recurses through the
/// children for the rest.
impl Ty {
    /// Convenience constructor for a type constructor from a static name.
    pub fn con(name: &'static str) -> Ty {
        Ty::Con(name.to_string())
    }

    /// The curried function type `p1 -> p2 -> … -> ret`.
    pub fn fun(params: &[Ty], ret: Ty) -> Ty {
        params.iter().rev().fold(ret, |acc, p| Ty::Fun(Box::new(p.clone()), Box::new(acc)))
    }

    /// `self` with `f` applied to each direct child type. A leaf (`Var`,
    /// `Con`, `Existential`, `Never`) is returned as it is; names, tags,
    /// lengths and `forall` binders are kept.
    pub fn map_children(&self, mut f: impl FnMut(&Ty) -> Ty) -> Ty {
        match self {
            Ty::Var(_) | Ty::Con(_) | Ty::Existential { .. } | Ty::Never => self.clone(),
            Ty::Fun(a, b) => Ty::Fun(Box::new(f(a)), Box::new(f(b))),
            Ty::App(head, args) => Ty::App(Box::new(f(head)), args.iter().map(&mut f).collect()),
            Ty::Sum { name, variants } => Ty::Sum {
                name: name.clone(),
                variants: variants.iter().map(|(n, p)| (n.clone(), p.map_types(&mut f))).collect(),
            },
            Ty::Constructor { owner, tag, arity } => Ty::Constructor {
                owner: Box::new(f(owner)),
                tag: *tag,
                arity: *arity,
            },
            Ty::Tuple(items) => Ty::Tuple(items.iter().map(f).collect()),
            Ty::Array { element, length } => Ty::Array {
                element: Box::new(f(element)),
                length: *length,
            },
            Ty::Record { fields } => Ty::Record {
                fields: fields.iter().map(|(n, t)| (n.clone(), f(t))).collect(),
            },
            Ty::Readonly(inner) => Ty::Readonly(Box::new(f(inner))),
            Ty::Forall { bounds, constraints, body } => Ty::Forall {
                bounds: bounds.clone(),
                constraints: constraints
                    .iter()
                    .map(|c| Constraint {
                        class: c.class.clone(),
                        args: c.args.iter().map(&mut f).collect(),
                    })
                    .collect(),
                body: Box::new(f(body)),
            },
        }
    }

    /// Whether `pred` holds for a direct child type of `self`, children in
    /// order (an `App`'s head before its arguments, a `forall`'s body before
    /// its constraint arguments).
    pub fn any_child(&self, mut pred: impl FnMut(&Ty) -> bool) -> bool {
        match self {
            Ty::Var(_) | Ty::Con(_) | Ty::Existential { .. } | Ty::Never => false,
            Ty::Fun(a, b) => pred(a) || pred(b),
            Ty::App(head, args) => pred(head) || args.iter().any(pred),
            Ty::Sum { variants, .. } => variants.iter().any(|(_, p)| p.types().any(&mut pred)),
            Ty::Constructor { owner: inner, .. } | Ty::Array { element: inner, .. } | Ty::Readonly(inner) => pred(inner),
            Ty::Tuple(items) => items.iter().any(pred),
            Ty::Record { fields } => fields.iter().any(|(_, t)| pred(t)),
            Ty::Forall { constraints, body, .. } => {
                pred(body) || constraints.iter().any(|c| c.args.iter().any(&mut pred))
            }
        }
    }

    /// Whether `pred` holds for every direct child type of `self`.
    pub fn all_children(&self, mut pred: impl FnMut(&Ty) -> bool) -> bool {
        !self.any_child(|child| !pred(child))
    }

    /// Visit each direct child type of `self`, in [`Ty::any_child`]'s order.
    pub fn for_each_child(&self, mut f: impl FnMut(&Ty)) {
        self.any_child(|child| {
            f(child);
            false
        });
    }

    /// Whether `pred` holds for `self` or any type inside it (pre-order,
    /// stopping at the first hit).
    pub fn any(&self, mut pred: impl FnMut(&Ty) -> bool) -> bool {
        self.any_by(&mut pred)
    }

    fn any_by<F: FnMut(&Ty) -> bool>(&self, pred: &mut F) -> bool {
        pred(self) || self.any_child(|child| child.any_by(pred))
    }

    /// Visit `self` and every type inside it, pre-order.
    pub fn walk(&self, mut f: impl FnMut(&Ty)) {
        self.any(|ty| {
            f(ty);
            false
        });
    }

    /// Whether a type variable appears anywhere in `self`. Blind to binders:
    /// a `forall`'s own variables count, so a rank-n type is open.
    pub fn has_var(&self) -> bool {
        self.any(|ty| matches!(ty, Ty::Var(_)))
    }

    /// Whether a free type variable appears in `self`: one no enclosing
    /// `forall` binds ([`ftv_ty`] without building the set).
    pub fn has_free_var(&self) -> bool {
        self.free_var_under(&mut Vec::new())
    }

    fn free_var_under(&self, bound: &mut Vec<TyVarId>) -> bool {
        match self {
            Ty::Var(v) => !bound.contains(v),
            Ty::Forall { bounds, .. } => {
                let depth = bound.len();
                bound.extend(bounds);
                let hit = self.any_child(|child| child.free_var_under(bound));
                bound.truncate(depth);
                hit
            }
            _ => self.any_child(|child| child.free_var_under(bound)),
        }
    }

    /// Append to `out` the type variables in `self` it does not hold yet,
    /// in order of first appearance (pre-order). Blind to binders, as
    /// [`Ty::has_var`] is.
    pub fn collect_vars(&self, out: &mut Vec<TyVarId>) {
        self.walk(|ty| {
            if let Ty::Var(v) = ty
                && !out.contains(v)
            {
                out.push(*v);
            }
        });
    }

    /// `self` with each free type variable `v` that `lookup` maps replaced
    /// by `lookup(v)`. Inside a `forall`, its bound variables are left alone.
    pub fn subst_vars<F: Fn(TyVarId) -> Option<Ty> + ?Sized>(&self, lookup: &F) -> Ty {
        match self {
            Ty::Var(v) => lookup(*v).unwrap_or_else(|| self.clone()),
            Ty::Forall { bounds, .. } => {
                let inner: &dyn Fn(TyVarId) -> Option<Ty> =
                    &|v| if bounds.contains(&v) { None } else { lookup(v) };
                self.map_children(|child| child.subst_vars(inner))
            }
            _ => self.map_children(|child| child.subst_vars(lookup)),
        }
    }
}

/// A typeclass constraint over one or more type arguments.
///
/// Unary binder bounds desugar as a single-arg constraint:
/// `T: Num` → `Constraint { class: "Num", args: [Var(α)] }`.
/// Multi-param classes use N args:
/// `Convert<A, B>` → `Constraint { class: "Convert", args: [Var(α), Var(β)] }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Constraint {
    pub class: String,
    pub args: Vec<Ty>,
}

impl Constraint {
    /// Unary constraint helper: `Class<T>` for a single type variable.
    pub fn unary(class: impl Into<String>, var: TyVarId) -> Self {
        Self {
            class: class.into(),
            args: vec![Ty::Var(var)],
        }
    }

    /// True when this is a unary constraint whose sole arg is `var`.
    pub fn is_unary_on(&self, var: TyVarId) -> bool {
        matches!(self.args.as_slice(), [Ty::Var(v)] if *v == var)
    }

    /// First type-variable argument, if any (unary / HKT head).
    pub fn primary_var(&self) -> Option<TyVarId> {
        match self.args.first() {
            Some(Ty::Var(v)) => Some(*v),
            Some(Ty::App(head, _)) => match head.as_ref() {
                Ty::Var(v) => Some(*v),
                _ => None,
            },
            _ => None,
        }
    }
}

impl std::fmt::Display for Constraint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.class)?;
        if !self.args.is_empty() {
            write!(f, "<")?;
            for (i, arg) in self.args.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}", arg)?;
            }
            write!(f, ">")?;
        }
        Ok(())
    }
}

/// A fresh type variable in a scheme that represents an associated-type
/// projection such as `T::Elem` or `T::Ref<A>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssocProjection {
    pub var: TyVarId,
    pub name: String,
    pub args: Vec<Ty>,
}

/// A type scheme: a type possibly quantified over some type variables,
/// with optional typeclass constraints on those variables.
///
/// `Scheme { bounds: vec![], kinds: vec![], constraints: vec![], ty: Var(α) }`
/// represents `α` (monomorphic). `bounds` lists the universally quantified
/// variables; `constraints` lists typeclass requirements on them (e.g. `T: Num`).
/// `kinds` (Phase 5) is parallel to `bounds` — empty means every bound has
/// kind `*`; otherwise `kinds[i]` is the kind of `bounds[i]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scheme {
    pub bounds: Vec<TyVarId>,
    /// Kinds of the quantified variables (parallel to `bounds`). Empty ≡ all `*`.
    pub kinds: Vec<super::kind::Kind>,
    /// Typeclass constraints on the quantified variables.
    pub constraints: Vec<Constraint>,
    /// Associated-type projection variables quantified by the scheme.
    pub assoc_projections: Vec<AssocProjection>,
    pub ty: Ty,
}

impl Scheme {
    /// Wrap a type as a monomorphic scheme (no quantified variables).
    pub fn mono(ty: Ty) -> Self {
        Self {
            bounds: Vec::new(),
            kinds: Vec::new(),
            constraints: Vec::new(),
            assoc_projections: Vec::new(),
            ty,
        }
    }

    /// Build a polymorphic scheme with constraints (all bounds kind `*`).
    pub fn poly(bounds: Vec<TyVarId>, constraints: Vec<Constraint>, ty: Ty) -> Self {
        let kinds = vec![super::kind::Kind::Type; bounds.len()];
        Self {
            bounds,
            kinds,
            constraints,
            assoc_projections: Vec::new(),
            ty,
        }
    }

    /// Build a polymorphic scheme with explicit kinds (Phase 5).
    pub fn poly_with_kinds(
        bounds: Vec<TyVarId>,
        kinds: Vec<super::kind::Kind>,
        constraints: Vec<Constraint>,
        ty: Ty,
    ) -> Self {
        debug_assert!(
            kinds.is_empty() || kinds.len() == bounds.len(),
            "kinds must be empty or parallel to bounds"
        );
        Self {
            bounds,
            kinds,
            constraints,
            assoc_projections: Vec::new(),
            ty,
        }
    }

    pub fn poly_with_kinds_and_assoc(
        bounds: Vec<TyVarId>,
        kinds: Vec<super::kind::Kind>,
        constraints: Vec<Constraint>,
        assoc_projections: Vec<AssocProjection>,
        ty: Ty,
    ) -> Self {
        debug_assert!(
            kinds.is_empty() || kinds.len() == bounds.len(),
            "kinds must be empty or parallel to bounds"
        );
        Self {
            bounds,
            kinds,
            constraints,
            assoc_projections,
            ty,
        }
    }

    /// Kind of quantified variable `i`, defaulting to `*` when unset.
    pub fn kind_at(&self, i: usize) -> super::kind::Kind {
        self.kinds
            .get(i)
            .cloned()
            .unwrap_or(super::kind::Kind::Type)
    }
}

/// Name of the `int` type constructor.
pub const INT: &str = "int";
/// Name of the `float` type constructor.
pub const FLOAT: &str = "float";
/// Name of the `string` type constructor.
pub const STRING: &str = "string";
/// Name of the `bool` type constructor.
pub const BOOL: &str = "bool";
/// Name of the `byte` type constructor (0–255; IO buffer element).
pub const BYTE: &str = "byte";
/// Name of the `unit` type constructor.
pub const UNIT: &str = "unit";
/// Name of the opaque `Stream` IO handle type.
pub const STREAM: &str = "Stream";
/// Opaque `thread` module handle types.
pub const THREAD: &str = "Thread";
pub const SENDER: &str = "Sender";
pub const RECEIVER: &str = "Receiver";
pub const MUTEX: &str = "Mutex";
pub const RWLOCK: &str = "RwLock";
/// Name of the `Root` type constructor (`gc::Root`).
pub const ROOT: &str = "Root";
/// Name of the `Weak` type constructor (`gc::Weak`).
pub const WEAK: &str = "Weak";

/// A builtin scalar type, as its `Ty::Con` names it: what the checker,
/// the HIR passes and the instance registry know about it.
#[derive(Debug)]
pub struct BuiltinScalar {
    pub name: &'static str,
    /// An immediate word: a copy, never a heap object (`string` is one and
    /// `unit` the shared empty tuple). A `mut` borrow of one binds a copy.
    pub word: bool,
    /// Nothing behind the value: no heap object it could share or change.
    pub no_heap: bool,
    /// May back a `#[repr(..)]` enum ([`ScalarBacking`]).
    pub repr_backing: bool,
    /// The scalars `x as T` and its builtin `Into` instance convert it to.
    pub casts_to: &'static [&'static str],
    /// The builtin trait instances it has, in registration order.
    pub traits: &'static [&'static str],
}

/// The builtin scalars. Their operators are the primitive ones: no user
/// `impl` of an operator trait applies to them.
pub const BUILTIN_SCALARS: [BuiltinScalar; 6] = [
    BuiltinScalar {
        name: INT,
        word: true,
        no_heap: true,
        repr_backing: true,
        casts_to: &[FLOAT, BYTE, BOOL],
        traits: &[
            "Add", "Sub", "Mul", "Div", "Neg", "Rem", "Pow", "Lt", "Le", "Gt", "Ge", "Num", "Ord", "Shl", "Shr", "BitAnd", "BitOr", "BitXor", "BitNot", "Integral", "Eq", "Show", "Hash",
            "Default",
        ],
    },
    BuiltinScalar {
        name: FLOAT,
        word: true,
        no_heap: true,
        repr_backing: true,
        casts_to: &[INT],
        traits: &[
            "Add", "Sub", "Mul", "Div", "Neg", "Rem", "Pow", "Lt", "Le", "Gt", "Ge", "Num", "Ord", "Eq", "Show", "Hash", "Default",
        ],
    },
    BuiltinScalar {
        name: STRING,
        word: false,
        no_heap: false,
        repr_backing: true,
        casts_to: &[],
        traits: &["Eq", "Show", "Lt", "Le", "Gt", "Ge", "Ord", "Length", "Hash", "Default"],
    },
    BuiltinScalar {
        name: BOOL,
        word: true,
        no_heap: true,
        repr_backing: true,
        casts_to: &[INT],
        traits: &["Eq", "Show", "Hash", "Default"],
    },
    BuiltinScalar {
        name: BYTE,
        word: true,
        no_heap: true,
        repr_backing: false,
        casts_to: &[INT],
        traits: &["Shl", "Shr", "BitAnd", "BitOr", "BitXor", "Eq", "Show", "Lt", "Le", "Gt", "Ge", "Ord", "Hash", "Default"],
    },
    BuiltinScalar {
        name: UNIT,
        word: false,
        no_heap: true,
        repr_backing: false,
        casts_to: &[],
        traits: &["Eq", "Show", "Hash"],
    },
];

impl BuiltinScalar {
    /// The builtin scalar `name` names, if it is one.
    pub fn named(name: &str) -> Option<&'static BuiltinScalar> {
        BUILTIN_SCALARS.iter().find(|s| s.name == name)
    }

    /// The builtin scalar `ty` is, if it is one (a bare `Ty::Con`).
    pub fn of(ty: &Ty) -> Option<&'static BuiltinScalar> {
        match ty {
            Ty::Con(name) => Self::named(name),
            _ => None,
        }
    }

    /// Whether `x as T` from this scalar to `to` is allowed (itself included).
    pub fn casts_to(&self, to: &str) -> bool {
        self.name == to || self.casts_to.contains(&to)
    }

    /// Whether `as` converts this scalar at all (`int`, `float`, `byte`, `bool`).
    pub fn castable(&self) -> bool {
        !self.casts_to.is_empty()
    }
}

/// The FFI integer types: opaque `Con`s by name, each one word.
pub const FFI_INTS: [&str; 7] = ["int8", "int16", "int32", "uint8", "uint16", "uint32", "uint64"];

/// The FFI pointer type, an opaque `Con` by name.
pub const FFI_PTR: &str = "ptr";

/// Whether `name` is a builtin scalar type ([`BUILTIN_SCALARS`]).
pub fn is_builtin_scalar(name: &str) -> bool {
    BuiltinScalar::named(name).is_some()
}

/// Build the `int` type.
pub fn int() -> Ty {
    Ty::Con(INT.into())
}

/// Build the `float` type.
pub fn float() -> Ty {
    Ty::Con(FLOAT.into())
}

/// Build the `string` type.
pub fn string() -> Ty {
    Ty::Con(STRING.into())
}

/// Build the `bool` type.
pub fn boolean() -> Ty {
    Ty::Con(BOOL.into())
}

/// Build the `byte` type (0–255).
pub fn byte() -> Ty {
    Ty::Con(BYTE.into())
}

/// Half-open lazy range constructor name (`Range<T>`).
pub const RANGE: &str = "Range";

/// Closed lazy range constructor name (`RangeInclusive<T>`).
pub const RANGE_INCLUSIVE: &str = "RangeInclusive";

/// Half-open lazy range type `Range<T>` (Phase P3).
pub fn range_ty(elem: Ty) -> Ty {
    Ty::App(Box::new(Ty::Con(RANGE.into())), vec![elem])
}

/// Closed lazy range type `RangeInclusive<T>` (Phase P3).
pub fn range_inclusive_ty(elem: Ty) -> Ty {
    Ty::App(Box::new(Ty::Con(RANGE_INCLUSIVE.into())), vec![elem])
}

/// Element type and inclusivity of `Range<T>` / `RangeInclusive<T>`.
pub fn range_app(ty: &Ty) -> Option<(&Ty, bool)> {
    match ty {
        Ty::App(head, args) if args.len() == 1 => match head.as_ref() {
            Ty::Con(n) if n == RANGE => Some((&args[0], false)),
            Ty::Con(n) if n == RANGE_INCLUSIVE => Some((&args[0], true)),
            _ => None,
        },
        Ty::Readonly(inner) => range_app(inner),
        _ => None,
    }
}

/// Build the opaque `Stream` type.
pub fn stream_ty() -> Ty {
    Ty::Con(STREAM.into())
}

/// Build the opaque `Thread<T>` join handle of a thread returning `T`
/// (#1905: `join` gives back the spawned function's own type).
pub fn thread_ty(value: Ty) -> Ty {
    Ty::App(Box::new(Ty::Con(THREAD.into())), vec![value])
}

/// Build the opaque channel `Sender<T>` type.
pub fn sender_ty(value: Ty) -> Ty {
    Ty::App(Box::new(Ty::Con(SENDER.into())), vec![value])
}

/// Build the opaque channel `Receiver<T>` type.
pub fn receiver_ty(value: Ty) -> Ty {
    Ty::App(Box::new(Ty::Con(RECEIVER.into())), vec![value])
}

/// Build the opaque `Mutex<T>` type.
pub fn mutex_ty(value: Ty) -> Ty {
    Ty::App(Box::new(Ty::Con(MUTEX.into())), vec![value])
}

/// Build the opaque `RwLock<T>` type.
pub fn rwlock_ty(value: Ty) -> Ty {
    Ty::App(Box::new(Ty::Con(RWLOCK.into())), vec![value])
}

/// The `thread` handle types that carry their value type.
pub const THREAD_HANDLES: [&str; 5] = [THREAD, SENDER, RECEIVER, MUTEX, RWLOCK];

/// The name of a builtin opaque handle `ty` is: a bare `Stream`, or a
/// `thread` handle applied to its value type (`Thread<T>`, `Sender<T>`, …).
pub fn opaque_handle_name(ty: &Ty) -> Option<&str> {
    match strip_readonly(ty) {
        Ty::Con(n) if n == STREAM => Some(n),
        Ty::App(head, args) if args.len() == 1 => match head.as_ref() {
            Ty::Con(n) if THREAD_HANDLES.contains(&n.as_str()) => Some(n),
            _ => None,
        },
        _ => None,
    }
}

/// Build the `unit` type (used for side-effecting expressions, `print`, etc.).
pub fn unit() -> Ty {
    Ty::Con(UNIT.into())
}

/// Build the checker-only bottom type for diverging expressions.
pub fn never() -> Ty {
    Ty::Never
}

/// Build `Root<T>` as a type application.
pub fn root_app_ty(inner: Ty) -> Ty {
    Ty::App(Box::new(Ty::Con(ROOT.into())), vec![inner])
}

/// Build `Weak<T>` as a type application.
pub fn weak_app_ty(inner: Ty) -> Ty {
    Ty::App(Box::new(Ty::Con(WEAK.into())), vec![inner])
}

/// Build `Vec<T>` as a type application.
pub fn vec_app_ty(inner: Ty) -> Ty {
    Ty::App(
        Box::new(Ty::Con(common::BUILTIN_VEC_TYPE.into())),
        vec![inner],
    )
}

/// One inherent method of the builtin `Vec<T>` (the receiver not counted
/// in `params`).
#[derive(Debug)]
pub struct VecMethod {
    pub name: &'static str,
    pub params: &'static [&'static str],
    /// Changes the `Vec`'s length: a write through a `readonly Vec<T>`,
    /// and a type error on a fixed `[T; N]`.
    pub mutates: bool,
}

/// The builtin `Vec<T>`'s inherent methods.
pub const VEC_METHODS: [VecMethod; 8] = [
    VecMethod { name: "push", params: &["x"], mutates: true },
    VecMethod { name: "pop", params: &[], mutates: true },
    VecMethod { name: "insert", params: &["i", "x"], mutates: true },
    VecMethod { name: "remove", params: &["i"], mutates: true },
    VecMethod { name: "clear", params: &[], mutates: true },
    VecMethod { name: "reserve", params: &["n"], mutates: true },
    VecMethod { name: "capacity", params: &[], mutates: false },
    VecMethod { name: "len", params: &[], mutates: false },
];

/// The builtin `Vec<T>` method `name`, if it is one.
pub fn vec_method(name: &str) -> Option<&'static VecMethod> {
    VEC_METHODS.iter().find(|m| m.name == name)
}

/// Element type of `Vec<T>`, if `ty` is (or peels to) that application.
pub fn vec_element_ty(ty: &Ty) -> Option<&Ty> {
    match ty {
        Ty::App(head, args) if args.len() == 1 => match head.as_ref() {
            Ty::Con(n) if n == common::BUILTIN_VEC_TYPE => Some(&args[0]),
            _ => None,
        },
        Ty::Readonly(inner) => vec_element_ty(inner),
        _ => None,
    }
}

/// `Option<T>`: the application `App(Con("Option"), [T])`. That is the one
/// form an `Option` or `Result` value's type has; a construct site refines
/// it to a `Constructor` owned by it. The enum's structural `Sum` is never a
/// value's type (a generic enum's constructors are owned by its
/// application).
pub fn option_ty(inner: Ty) -> Ty {
    Ty::App(
        Box::new(Ty::Con(common::BUILTIN_OPTION_ENUM.into())),
        vec![inner],
    )
}

/// `Result<T, E>`, as [`option_ty`].
pub fn result_ty(ok: Ty, err: Ty) -> Ty {
    Ty::App(
        Box::new(Ty::Con(common::BUILTIN_RESULT_ENUM.into())),
        vec![ok, err],
    )
}

impl Ty {
    /// `self` without a variant-tag refinement: a `Constructor`'s owner.
    fn enum_owner(&self) -> &Ty {
        match self {
            Ty::Constructor { owner, .. } => owner.enum_owner(),
            other => other,
        }
    }

    /// The arguments of `self` as an application of the builtin enum `name`.
    fn builtin_enum_args(&self, name: &str) -> Option<&[Ty]> {
        match self.enum_owner() {
            Ty::App(head, args) if matches!(head.as_ref(), Ty::Con(n) if n == name) => Some(args),
            _ => None,
        }
    }

    /// `T` of an `Option<T>` (or of one of its variants' constructor types).
    pub fn as_option(&self) -> Option<&Ty> {
        self.builtin_enum_args(common::BUILTIN_OPTION_ENUM)?.first()
    }

    /// `(T, E)` of a `Result<T, E>` (or of one of its variants' constructor
    /// types).
    pub fn as_result(&self) -> Option<(&Ty, &Ty)> {
        match self.builtin_enum_args(common::BUILTIN_RESULT_ENUM)? {
            [ok, err, ..] => Some((ok, err)),
            _ => None,
        }
    }
}

/// Extract `T` from `Option<T>`.
pub fn option_inner(ty: &Ty) -> Option<Ty> {
    ty.as_option().cloned()
}

/// Extract `(T, E)` from `Result<T, E>`.
pub fn result_ok_err(ty: &Ty) -> Option<(Ty, Ty)> {
    ty.as_result().map(|(ok, err)| (ok.clone(), err.clone()))
}

/// An enum value's type as an annotation names it: a variant's constructor
/// type (`Dir::Down(3)`, `Option::Some(1)`) is its owner's, and a user
/// enum's structural sum is `Con("Dir")`. Other types are returned as they
/// are.
///
/// A polymorphic parameter must not be pinned to one variant tag, so
/// `min(Rank::Mid, Rank::Low)` unifies as one `T`; comparisons, instance
/// lookups and derive-`Ord` keys go by the same type.
pub fn nominal_enum(ty: Ty) -> Ty {
    match ty {
        Ty::Constructor { owner, .. } => nominal_enum(*owner),
        Ty::Sum { name, .. } => Ty::Con(name),
        other => other,
    }
}

/// [`nominal_enum`] at every level: `(Some(3), 1)`'s type is
/// `(Option<int>, int)`, the shape its structural instance is of (#2607).
pub fn nominal_enum_deep(ty: &Ty) -> Ty {
    match ty {
        Ty::Constructor { owner, .. } => nominal_enum_deep(owner),
        Ty::Sum { name, .. } => Ty::Con(name.clone()),
        _ => ty.map_children(nominal_enum_deep),
    }
}

/// `Option<T>` / `Result<T, E>` behind a variant's constructor type as the
/// application, the type an instance lookup matches (#1479). `None` for any
/// other type.
pub fn builtin_enum_app(ty: &Ty) -> Option<Ty> {
    (ty.as_option().is_some() || ty.as_result().is_some()).then(|| ty.enum_owner().clone())
}

/// True when `ty` is the built-in `Option`, applied or as its bare
/// constructor.
pub fn is_option_ty(ty: &Ty) -> bool {
    match ty.enum_owner() {
        Ty::Con(name) => name == common::BUILTIN_OPTION_ENUM,
        owner => owner.builtin_enum_args(common::BUILTIN_OPTION_ENUM).is_some(),
    }
}

/// True when `ty` is the built-in `Result`, applied or as its bare
/// constructor.
pub fn is_result_ty(ty: &Ty) -> bool {
    match ty.enum_owner() {
        Ty::Con(name) => name == common::BUILTIN_RESULT_ENUM,
        owner => owner.builtin_enum_args(common::BUILTIN_RESULT_ENUM).is_some(),
    }
}

/// Build the `(T1, T2, ..., Tn)` heterogeneous tuple type. The empty
/// tuple `()` is the `unit` type: one type, one layout.
pub fn tuple(tys: Vec<Ty>) -> Ty {
    if tys.is_empty() {
        return unit();
    }
    Ty::Tuple(tys)
}

/// Build the `readonly T` wrapper type.
pub fn readonly_ty(ty: Ty) -> Ty {
    Ty::Readonly(Box::new(ty))
}

/// Peel `readonly` qualifiers for read/mutation checks.
pub fn strip_readonly(ty: &Ty) -> &Ty {
    match ty {
        Ty::Readonly(inner) => strip_readonly(inner),
        other => other,
    }
}

/// `ty` with every `readonly` qualifier in it peeled: the type a clone is
/// keyed and built at, since `readonly` has no runtime form (#1555, #1806).
pub fn strip_readonly_deep(ty: &Ty) -> Ty {
    match ty {
        Ty::Readonly(inner) => strip_readonly_deep(inner),
        _ => ty.map_children(strip_readonly_deep),
    }
}

/// Whether `name` is a builtin opaque handle: an IO `Stream` or a
/// `thread` handle. It has no fields, indices or `push` a write could go
/// through; what it refers to changes only through its own host calls.
pub fn is_opaque_handle(name: &str) -> bool {
    [STREAM, THREAD, SENDER, RECEIVER, MUTEX, RWLOCK].contains(&name)
}

/// Whether a `const` binding should emit a shallow-immutability warning.
/// A `readonly` value is frozen all the way down (#1552), and an opaque
/// handle has nothing to mutate through (#1709). A GC `Root<T>` /
/// `Weak<T>` handle warns as its target `T` does. The checker looks
/// through enums first (`Checker::const_shallow_mutable`, #1714), since
/// whether one holds mutable state depends on its declared payloads.
pub fn is_shallow_const_mutable(ty: &Ty) -> bool {
    match ty {
        Ty::Readonly(_) => false,
        Ty::Array { .. } | Ty::Record { .. } => true,
        Ty::Con(name) => !is_builtin_scalar(name) && !is_opaque_handle(name),
        Ty::App(head, args) => match (head.as_ref(), args.as_slice()) {
            (Ty::Con(n), [target]) if n == ROOT || n == WEAK => is_shallow_const_mutable(target),
            (Ty::Con(n), [_]) if THREAD_HANDLES.contains(&n.as_str()) => false,
            _ => true,
        },
        _ => false,
    }
}

/// Build the `[T]` (dynamic-length) array type.
pub fn array(element: Ty) -> Ty {
    Ty::Array {
        element: Box::new(element),
        length: ArrayLength::Dynamic,
    }
}

/// Build the `[T; N]` (fixed-length) array type.
pub fn array_fixed(element: Ty, length: usize) -> Ty {
    Ty::Array {
        element: Box::new(element),
        length: ArrayLength::Static(length),
    }
}

/// Build the `{ name: T, ... }` anonymous record type.
pub fn record(fields: Vec<(String, Ty)>) -> Ty {
    Ty::Record { fields }
}

/// Substitute named type-parameter placeholders (`Ty::Con("T")`) in a type.
///
/// Used by polymorphic enum construct/match to freshen schema payloads
/// stored with `Con(param)` markers into site-local type variables.
pub fn subst_ty_params(ty: &Ty, params: &std::collections::HashMap<String, Ty>) -> Ty {
    match ty {
        Ty::Con(name) => params.get(name).cloned().unwrap_or_else(|| ty.clone()),
        _ => ty.map_children(|t| subst_ty_params(t, params)),
    }
}

/// Rewrite in-scope type-parameter variables to `Ty::Con(name)` schema
/// markers for storage in the enum payload registry.
pub fn schemaize_ty(ty: &Ty, var_to_name: &std::collections::HashMap<TyVarId, String>) -> Ty {
    ty.subst_vars(&|v| var_to_name.get(&v).map(|n| Ty::Con(n.clone())))
}

/// Free type variables of a `Ty`: a `forall`'s own variables are bound.
pub fn ftv_ty(ty: &Ty) -> HashSet<TyVarId> {
    let mut acc = HashSet::new();
    collect_free_vars(ty, &mut acc);
    acc
}

fn collect_free_vars(ty: &Ty, acc: &mut HashSet<TyVarId>) {
    match ty {
        Ty::Var(v) => {
            acc.insert(*v);
        }
        Ty::Forall { bounds, .. } => {
            let mut inner = HashSet::new();
            ty.for_each_child(|child| collect_free_vars(child, &mut inner));
            acc.extend(inner.into_iter().filter(|v| !bounds.contains(v)));
        }
        _ => ty.for_each_child(|child| collect_free_vars(child, acc)),
    }
}

/// Free type variables of a `Scheme` (excluding the quantified ones).
pub fn ftv_scheme(s: &Scheme) -> HashSet<TyVarId> {
    let mut acc = ftv_ty(&s.ty);
    for projection in &s.assoc_projections {
        acc.insert(projection.var);
        for arg in &projection.args {
            acc.extend(ftv_ty(arg));
        }
    }
    let bound: HashSet<_> = s.bounds.iter().copied().collect();
    acc.retain(|v| !bound.contains(v));
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(i: u32) -> Ty {
        Ty::Var(TyVarId(i))
    }

    fn forall(bound: u32, body: Ty) -> Ty {
        Ty::Forall {
            bounds: vec![TyVarId(bound)],
            constraints: vec![Constraint::unary("Show", TyVarId(bound))],
            body: Box::new(body),
        }
    }

    fn sum(payload: Ty) -> Ty {
        Ty::Sum {
            name: "Pick".to_string(),
            variants: vec![
                ("One".to_string(), EnumVariantPayloadTy::Tuple(vec![payload.clone()])),
                ("Two".to_string(), EnumVariantPayloadTy::Record(vec![("a".to_string(), payload)])),
                ("Zero".to_string(), EnumVariantPayloadTy::Unit),
            ],
        }
    }

    #[test]
    fn map_children_reaches_payloads_and_forall_constraints() {
        let to_int = |t: &Ty| if matches!(t, Ty::Var(_)) { int() } else { t.clone() };
        assert_eq!(sum(v(0)).map_children(to_int), sum(int()));
        let Ty::Forall { constraints, body, .. } = forall(1, v(1)).map_children(to_int) else {
            panic!("forall kept")
        };
        assert_eq!(*body, int());
        assert_eq!(constraints[0].args, vec![int()]);
    }

    #[test]
    fn any_and_collect_vars_see_every_shape() {
        let record = record(vec![("x".to_string(), v(2))]);
        let ty = Ty::Tuple(vec![sum(v(0)), forall(1, v(1)), record, v(0)]);
        assert!(sum(v(0)).has_var());
        let mut vars = Vec::new();
        ty.collect_vars(&mut vars);
        assert_eq!(vars, vec![TyVarId(0), TyVarId(1), TyVarId(2)]);
    }

    #[test]
    fn free_vars_skip_a_foralls_own() {
        let ty = forall(1, Ty::Fun(Box::new(v(1)), Box::new(v(0))));
        assert!(ty.has_var());
        assert!(ty.has_free_var());
        assert_eq!(ftv_ty(&ty), HashSet::from([TyVarId(0)]));
        assert!(!forall(1, v(1)).has_free_var());
        // A variable free outside the `forall` stays free.
        let outer = Ty::Tuple(vec![v(1), forall(1, v(1))]);
        assert_eq!(ftv_ty(&outer), HashSet::from([TyVarId(1)]));
    }

    #[test]
    fn subst_vars_leaves_bound_vars_alone() {
        let ty = Ty::Tuple(vec![v(1), forall(1, v(1))]);
        let got = ty.subst_vars(&|var| (var == TyVarId(1)).then(int));
        assert_eq!(got, Ty::Tuple(vec![int(), forall(1, v(1))]));
    }

    #[test]
    fn ftv_of_var_is_just_that_var() {
        assert_eq!(ftv_ty(&v(0)), HashSet::from([TyVarId(0)]));
    }

    #[test]
    fn ftv_of_con_is_empty() {
        assert!(ftv_ty(&int()).is_empty());
        assert!(ftv_ty(&float()).is_empty());
    }

    #[test]
    fn ftv_of_fun_is_union_of_args() {
        let ty = Ty::Fun(Box::new(v(0)), Box::new(v(1)));
        assert_eq!(ftv_ty(&ty), HashSet::from([TyVarId(0), TyVarId(1)]));
    }

    #[test]
    fn ftv_of_app_walks_args() {
        let ty = Ty::App(Box::new(Ty::Con("Foo".into())), vec![v(0), v(2)]);
        assert_eq!(ftv_ty(&ty), HashSet::from([TyVarId(0), TyVarId(2)]));
    }

    #[test]
    fn ftv_of_array_walks_inner() {
        let ty = array(v(3));
        assert_eq!(ftv_ty(&ty), HashSet::from([TyVarId(3)]));
    }

    #[test]
    fn ftv_of_nested_fun_dedups() {
        let ty = Ty::Fun(
            Box::new(v(0)),
            Box::new(Ty::Fun(Box::new(v(0)), Box::new(v(1)))),
        );
        assert_eq!(ftv_ty(&ty), HashSet::from([TyVarId(0), TyVarId(1)]));
    }

    #[test]
    fn ftv_of_scheme_excludes_bounds() {
        let scheme = Scheme {
            bounds: vec![TyVarId(0)],
            kinds: vec![],
            constraints: vec![],
            assoc_projections: vec![],
            ty: v(0),
        };
        assert!(ftv_scheme(&scheme).is_empty());
    }

    #[test]
    fn ftv_of_scheme_keeps_non_bound_vars() {
        let scheme = Scheme {
            bounds: vec![TyVarId(0)],
            kinds: vec![],
            constraints: vec![],
            assoc_projections: vec![],
            ty: Ty::Fun(Box::new(v(0)), Box::new(v(1))),
        };
        assert_eq!(ftv_scheme(&scheme), HashSet::from([TyVarId(1)]));
    }

    #[test]
    fn scheme_mono_has_no_bounds() {
        let s = Scheme::mono(int());
        assert!(s.bounds.is_empty());
        assert_eq!(s.ty, int());
    }

    #[test]
    fn ftv_of_sum_walks_variant_payloads() {
        // enum E { A(int), B(string) }  — ftv is the union of the
        // payload free variables (here, just the vars inside the
        // payloads).
        let sum = Ty::Sum {
            name: "E".into(),
            variants: vec![
                ("A".into(), EnumVariantPayloadTy::Tuple(vec![v(0)])),
                ("B".into(), EnumVariantPayloadTy::Tuple(vec![string()])),
            ],
        };
        assert_eq!(ftv_ty(&sum), HashSet::from([TyVarId(0)]));
    }

    #[test]
    fn ftv_of_constructor_walks_owner() {
        // The owner of a Constructor is the parent sum. Its ftv is
        // the union of the owner's variant-payload ftvs.
        let sum = Ty::Sum {
            name: "E".into(),
            variants: vec![("A".into(), EnumVariantPayloadTy::Tuple(vec![v(1), v(2)]))],
        };
        let ctor = Ty::Constructor {
            owner: Box::new(sum),
            tag: 0,
            arity: 2,
        };
        assert_eq!(ftv_ty(&ctor), HashSet::from([TyVarId(1), TyVarId(2)]));
    }

    #[test]
    fn ftv_of_recursive_sum_is_empty_when_payloads_use_con() {
        // `enum Tree { Leaf, Node(int, Tree, Tree) }` — the
        // recursive reference inside `Node` is `Ty::Con("Tree")`
        // (the isorecursive encoding, see `register_enum`), which
        // has no free vars. So the entire sum has no free vars.
        let tree = Ty::Con("Tree".into());
        let sum = Ty::Sum {
            name: "Tree".into(),
            variants: vec![
                ("Leaf".into(), EnumVariantPayloadTy::Unit),
                (
                    "Node".into(),
                    EnumVariantPayloadTy::Tuple(vec![int(), tree.clone(), tree]),
                ),
            ],
        };
        assert!(ftv_ty(&sum).is_empty());
    }

    #[test]
    fn payload_field_count_unit() {
        assert_eq!(EnumVariantPayloadTy::Unit.field_count(), 0);
    }

    #[test]
    fn payload_field_count_tuple() {
        let p = EnumVariantPayloadTy::Tuple(vec![int(), string()]);
        assert_eq!(p.field_count(), 2);
    }

    #[test]
    fn payload_field_count_record() {
        let p = EnumVariantPayloadTy::Record(vec![("x".into(), int()), ("y".into(), string())]);
        assert_eq!(p.field_count(), 2);
    }

    #[test]
    fn payload_field_types_unit() {
        assert!(EnumVariantPayloadTy::Unit.field_types().is_empty());
    }

    #[test]
    fn payload_field_types_tuple() {
        let p = EnumVariantPayloadTy::Tuple(vec![int(), string()]);
        assert_eq!(p.field_types(), vec![&int(), &string()]);
    }

    #[test]
    fn payload_field_types_record() {
        let p = EnumVariantPayloadTy::Record(vec![("x".into(), int()), ("y".into(), string())]);
        assert_eq!(p.field_types(), vec![&int(), &string()]);
    }

    #[test]
    fn payload_field_pairs_unit() {
        assert!(EnumVariantPayloadTy::Unit.field_pairs().is_empty());
    }

    #[test]
    fn payload_field_pairs_tuple_uses_synthetic_names() {
        // Tuple `Foo(int, int)` → field_pairs returns
        // `[("0", int), ("1", int)]` (synthetic names — used by
        // codegen reordering). This is the ONLY place the
        // synthetic-name trick is applied.
        let p = EnumVariantPayloadTy::Tuple(vec![int(), int()]);
        assert_eq!(
            p.field_pairs(),
            vec![("0".into(), int()), ("1".into(), int())]
        );
    }

    #[test]
    fn payload_field_pairs_record_keeps_declared_names() {
        let p = EnumVariantPayloadTy::Record(vec![("x".into(), int()), ("y".into(), int())]);
        assert_eq!(
            p.field_pairs(),
            vec![("x".into(), int()), ("y".into(), int())]
        );
    }

    #[test]
    fn range_app_distinguishes_half_open_and_inclusive() {
        let half = range_ty(int());
        assert_eq!(
            range_app(&half),
            Some((&int(), false))
        );
        let closed = range_inclusive_ty(float());
        assert_eq!(
            range_app(&closed).map(|(e, inc)| (format!("{e}"), inc)),
            Some(("float".into(), true))
        );
    }

    #[test]
    fn range_app_peels_readonly() {
        let ty = readonly_ty(range_ty(byte()));
        let (elem, inclusive) = range_app(&ty).expect("readonly Range");
        assert!(!inclusive);
        assert_eq!(elem, &byte());
    }

    #[test]
    fn range_app_rejects_non_range() {
        assert!(range_app(&int()).is_none());
        assert!(range_app(&vec_app_ty(int())).is_none());
    }
}
