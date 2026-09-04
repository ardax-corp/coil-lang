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
    List(Box<Ty>),
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

impl Ty {
    /// Convenience constructor for a type constructor from a static name.
    pub fn con(name: &'static str) -> Ty {
        Ty::Con(name.to_string())
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

// --- Built-in type constructors (as static name strings) ---

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
/// Name of the `List` type constructor.
#[allow(dead_code)] // name reserved; list types still use Ty::List, not Ty::Con(LIST)
pub const LIST: &str = "List";

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

/// Build the opaque `Thread` join-handle type.
pub fn thread_ty() -> Ty {
    Ty::Con(THREAD.into())
}

/// Build the opaque channel `Sender` type.
pub fn sender_ty() -> Ty {
    Ty::Con(SENDER.into())
}

/// Build the opaque channel `Receiver` type.
pub fn receiver_ty() -> Ty {
    Ty::Con(RECEIVER.into())
}

/// Build the opaque `Mutex` type.
pub fn mutex_ty() -> Ty {
    Ty::Con(MUTEX.into())
}

/// Build the opaque `RwLock` type.
pub fn rwlock_ty() -> Ty {
    Ty::Con(RWLOCK.into())
}

/// Build the `unit` type (used for side-effecting expressions, `print`, etc.).
pub fn unit() -> Ty {
    Ty::Con(UNIT.into())
}

/// Build the checker-only bottom type for diverging expressions.
pub fn never() -> Ty {
    Ty::Never
}

/// Build `Option<T>` as a type application.
pub fn option_app_ty(inner: Ty) -> Ty {
    Ty::App(
        Box::new(Ty::Con(common::BUILTIN_OPTION_ENUM.into())),
        vec![inner],
    )
}

/// Build `Result<T, E>` as a type application.
pub fn result_app_ty(ok: Ty, err: Ty) -> Ty {
    Ty::App(
        Box::new(Ty::Con(common::BUILTIN_RESULT_ENUM.into())),
        vec![ok, err],
    )
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

/// Drop variant-tag refinement, yielding the owning enum type.
///
/// Construct sites infer `Ty::Constructor { tag, owner, … }`. Comparisons
/// already peel to the parent; polymorphic params / mixed-variant joins must
/// do the same so `min(Rank::Mid, Rank::Low)` unifies as one `T`.
///
/// Monomorphic enums collapse to `Ty::Con(name)` (same shape as a `: Rank`
/// annotation and as derive-Ord instance keys). Builtin `Option`/`Result`
/// keep their structural `Sum` so payload types are preserved.
pub fn peel_constructor_refinement(ty: Ty) -> Ty {
    match ty {
        Ty::Constructor { owner, .. } => peel_constructor_refinement(*owner),
        Ty::Sum { name, .. } if !common::is_poly_builtin_enum(&name) => Ty::Con(name),
        other => other,
    }
}

/// Build `Option<T>` as a sum type (`None` | `Some(T)`).
///
/// The builtin annotation path uses [`option_app_ty`]. This structural
/// form is still used by constructor owners and match metadata.
pub fn option_ty(inner: Ty) -> Ty {
    Ty::Sum {
        name: common::BUILTIN_OPTION_ENUM.into(),
        variants: vec![
            ("None".into(), EnumVariantPayloadTy::Unit),
            ("Some".into(), EnumVariantPayloadTy::Tuple(vec![inner])),
        ],
    }
}

/// Build `Result<T, E>` as a sum type (`Ok(T)` | `Err(E)`).
///
/// The builtin annotation path uses [`result_app_ty`]. This structural
/// form is still used by constructor owners and match metadata.
pub fn result_ty(ok: Ty, err: Ty) -> Ty {
    Ty::Sum {
        name: common::BUILTIN_RESULT_ENUM.into(),
        variants: vec![
            ("Ok".into(), EnumVariantPayloadTy::Tuple(vec![ok])),
            ("Err".into(), EnumVariantPayloadTy::Tuple(vec![err])),
        ],
    }
}

/// Extract `T` from `Option<T>` (Sum or Constructor owner).
pub fn option_inner(ty: &Ty) -> Option<Ty> {
    match ty {
        Ty::App(con, args) if matches!(con.as_ref(), Ty::Con(name) if name == common::BUILTIN_OPTION_ENUM) => {
            args.first().cloned()
        }
        Ty::Sum { name, variants } if name == common::BUILTIN_OPTION_ENUM => {
            for (vn, payload) in variants {
                if vn == "Some" {
                    return payload.field_types().into_iter().next().cloned();
                }
            }
            None
        }
        Ty::Constructor { owner, .. } => option_inner(owner),
        Ty::Con(name) if name == common::BUILTIN_OPTION_ENUM => None,
        _ => None,
    }
}

/// Extract `(T, E)` from `Result<T, E>`.
pub fn result_ok_err(ty: &Ty) -> Option<(Ty, Ty)> {
    match ty {
        Ty::App(con, args) if matches!(con.as_ref(), Ty::Con(name) if name == common::BUILTIN_RESULT_ENUM) => {
            match (args.first(), args.get(1)) {
                (Some(ok), Some(err)) => Some((ok.clone(), err.clone())),
                _ => None,
            }
        }
        Ty::Sum { name, variants } if name == common::BUILTIN_RESULT_ENUM => {
            let mut ok = None;
            let mut err = None;
            for (vn, payload) in variants {
                let field = payload.field_types().into_iter().next().cloned();
                if vn == "Ok" {
                    ok = field;
                } else if vn == "Err" {
                    err = field;
                }
            }
            match (ok, err) {
                (Some(o), Some(e)) => Some((o, e)),
                _ => None,
            }
        }
        Ty::Constructor { owner, .. } => result_ok_err(owner),
        _ => None,
    }
}

/// True when `ty` is (or owns) the built-in `Option` sum.
pub fn is_option_ty(ty: &Ty) -> bool {
    match ty {
        Ty::Sum { name, .. } | Ty::Con(name) => name == common::BUILTIN_OPTION_ENUM,
        Ty::App(con, _) => {
            matches!(con.as_ref(), Ty::Con(name) if name == common::BUILTIN_OPTION_ENUM)
        }
        Ty::Constructor { owner, .. } => is_option_ty(owner),
        _ => false,
    }
}

/// True when `ty` is (or owns) the built-in `Result` sum.
pub fn is_result_ty(ty: &Ty) -> bool {
    match ty {
        Ty::Sum { name, .. } | Ty::Con(name) => name == common::BUILTIN_RESULT_ENUM,
        Ty::App(con, _) => {
            matches!(con.as_ref(), Ty::Con(name) if name == common::BUILTIN_RESULT_ENUM)
        }
        Ty::Constructor { owner, .. } => is_result_ty(owner),
        _ => false,
    }
}

/// Build the `List<t>` type.
pub fn list(inner: Ty) -> Ty {
    Ty::List(Box::new(inner))
}

/// Build the `(T1, T2, ..., Tn)` heterogeneous tuple type.
pub fn tuple(tys: Vec<Ty>) -> Ty {
    Ty::Tuple(tys)
}

/// Build the `readonly T` wrapper type.
pub fn readonly_ty(ty: Ty) -> Ty {
    Ty::Readonly(Box::new(ty))
}

/// Peel `readonly` qualifiers for read/mutation checks.
pub fn strip_readonly<'a>(ty: &'a Ty) -> &'a Ty {
    match ty {
        Ty::Readonly(inner) => strip_readonly(inner),
        other => other,
    }
}

/// Whether a `const` binding should emit a shallow-immutability warning.
pub fn is_shallow_const_mutable(ty: &Ty) -> bool {
    match strip_readonly(ty) {
        Ty::Array { .. } | Ty::Record { .. } => true,
        Ty::Con(name) => {
            name != "int"
                && name != "float"
                && name != "string"
                && name != "bool"
                && name != "byte"
                && name != "unit"
        }
        Ty::App(_, _) => true,
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
        Ty::Var(_) => ty.clone(),
        Ty::Fun(a, b) => Ty::Fun(
            Box::new(subst_ty_params(a, params)),
            Box::new(subst_ty_params(b, params)),
        ),
        Ty::App(con, args) => Ty::App(
            Box::new(subst_ty_params(con, params)),
            args.iter().map(|a| subst_ty_params(a, params)).collect(),
        ),
        Ty::List(inner) => Ty::List(Box::new(subst_ty_params(inner, params))),
        Ty::Sum { name, variants } => Ty::Sum {
            name: name.clone(),
            variants: variants
                .iter()
                .map(|(n, p)| (n.clone(), subst_payload_params(p, params)))
                .collect(),
        },
        Ty::Constructor { owner, tag, arity } => Ty::Constructor {
            owner: Box::new(subst_ty_params(owner, params)),
            tag: *tag,
            arity: *arity,
        },
        Ty::Tuple(tys) => Ty::Tuple(tys.iter().map(|t| subst_ty_params(t, params)).collect()),
        Ty::Array { element, length } => Ty::Array {
            element: Box::new(subst_ty_params(element, params)),
            length: *length,
        },
        Ty::Record { fields } => Ty::Record {
            fields: fields
                .iter()
                .map(|(n, t)| (n.clone(), subst_ty_params(t, params)))
                .collect(),
        },
        Ty::Existential { .. } => ty.clone(),
        Ty::Forall {
            bounds,
            constraints,
            body,
        } => Ty::Forall {
            bounds: bounds.clone(),
            constraints: constraints.clone(),
            body: Box::new(subst_ty_params(body, params)),
        },
        Ty::Readonly(inner) => Ty::Readonly(Box::new(subst_ty_params(inner, params))),
        Ty::Never => Ty::Never,
    }
}

/// Substitute named type-parameter placeholders in a variant payload.
pub fn subst_payload_params(
    payload: &EnumVariantPayloadTy,
    params: &std::collections::HashMap<String, Ty>,
) -> EnumVariantPayloadTy {
    match payload {
        EnumVariantPayloadTy::Unit => EnumVariantPayloadTy::Unit,
        EnumVariantPayloadTy::Tuple(tys) => {
            EnumVariantPayloadTy::Tuple(tys.iter().map(|t| subst_ty_params(t, params)).collect())
        }
        EnumVariantPayloadTy::Record(fields) => EnumVariantPayloadTy::Record(
            fields
                .iter()
                .map(|(n, t)| (n.clone(), subst_ty_params(t, params)))
                .collect(),
        ),
    }
}

/// Rewrite in-scope type-parameter variables to `Ty::Con(name)` schema
/// markers for storage in the enum payload registry.
pub fn schemaize_ty(ty: &Ty, var_to_name: &std::collections::HashMap<TyVarId, String>) -> Ty {
    match ty {
        Ty::Var(id) => var_to_name
            .get(id)
            .map(|n| Ty::Con(n.clone()))
            .unwrap_or_else(|| ty.clone()),
        Ty::Con(_) | Ty::Existential { .. } | Ty::Never => ty.clone(),
        Ty::Fun(a, b) => Ty::Fun(
            Box::new(schemaize_ty(a, var_to_name)),
            Box::new(schemaize_ty(b, var_to_name)),
        ),
        Ty::App(con, args) => Ty::App(
            Box::new(schemaize_ty(con, var_to_name)),
            args.iter().map(|a| schemaize_ty(a, var_to_name)).collect(),
        ),
        Ty::List(inner) => Ty::List(Box::new(schemaize_ty(inner, var_to_name))),
        Ty::Sum { name, variants } => Ty::Sum {
            name: name.clone(),
            variants: variants
                .iter()
                .map(|(n, p)| (n.clone(), schemaize_payload(p, var_to_name)))
                .collect(),
        },
        Ty::Constructor { owner, tag, arity } => Ty::Constructor {
            owner: Box::new(schemaize_ty(owner, var_to_name)),
            tag: *tag,
            arity: *arity,
        },
        Ty::Tuple(tys) => Ty::Tuple(tys.iter().map(|t| schemaize_ty(t, var_to_name)).collect()),
        Ty::Array { element, length } => Ty::Array {
            element: Box::new(schemaize_ty(element, var_to_name)),
            length: *length,
        },
        Ty::Record { fields } => Ty::Record {
            fields: fields
                .iter()
                .map(|(n, t)| (n.clone(), schemaize_ty(t, var_to_name)))
                .collect(),
        },
        Ty::Forall {
            bounds,
            constraints,
            body,
        } => Ty::Forall {
            bounds: bounds.clone(),
            constraints: constraints.clone(),
            body: Box::new(schemaize_ty(body, var_to_name)),
        },
        Ty::Readonly(inner) => Ty::Readonly(Box::new(schemaize_ty(inner, var_to_name))),
    }
}

/// Schemaize a variant payload (type-param vars → `Con(name)`).
pub fn schemaize_payload(
    payload: &EnumVariantPayloadTy,
    var_to_name: &std::collections::HashMap<TyVarId, String>,
) -> EnumVariantPayloadTy {
    match payload {
        EnumVariantPayloadTy::Unit => EnumVariantPayloadTy::Unit,
        EnumVariantPayloadTy::Tuple(tys) => {
            EnumVariantPayloadTy::Tuple(tys.iter().map(|t| schemaize_ty(t, var_to_name)).collect())
        }
        EnumVariantPayloadTy::Record(fields) => EnumVariantPayloadTy::Record(
            fields
                .iter()
                .map(|(n, t)| (n.clone(), schemaize_ty(t, var_to_name)))
                .collect(),
        ),
    }
}

// --- Free type variables ---

/// Free type variables of a `Ty`.
pub fn ftv_ty(ty: &Ty) -> HashSet<TyVarId> {
    let mut acc = HashSet::new();
    go(ty, &mut acc);
    acc
}

fn go(ty: &Ty, acc: &mut HashSet<TyVarId>) {
    match ty {
        Ty::Var(v) => {
            acc.insert(*v);
        }
        Ty::Con(_) | Ty::Existential { .. } | Ty::Never => {}
        Ty::Fun(a, b) => {
            go(a, acc);
            go(b, acc);
        }
        Ty::App(head, args) => {
            // Phase 5: HKT heads may be `Ty::Var(F)` — must collect F.
            go(head, acc);
            for a in args {
                go(a, acc);
            }
        }
        Ty::List(inner) => {
            go(inner, acc);
        }
        // Recursive enum payloads use Ty::Con(name) (isorecursive encoding).
        Ty::Sum { variants, .. } => {
            for (_, payload) in variants {
                for p in payload.field_types() {
                    go(p, acc);
                }
            }
        }
        Ty::Constructor { owner, .. } => {
            go(owner, acc);
        }
        Ty::Tuple(tys) => {
            for t in tys {
                go(t, acc);
            }
        }
        Ty::Array { element, .. } => {
            go(element, acc);
        }
        Ty::Record { fields } => {
            for (_, fty) in fields {
                go(fty, acc);
            }
        }
        Ty::Forall {
            bounds,
            constraints,
            body,
        } => {
            go(body, acc);
            for c in constraints {
                for a in &c.args {
                    go(a, acc);
                }
            }
            let bound: HashSet<_> = bounds.iter().copied().collect();
            acc.retain(|v| !bound.contains(v));
        }
        Ty::Readonly(inner) => go(inner, acc),
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
    fn ftv_of_list_walks_inner() {
        let ty = list(v(3));
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

    // ---- Sum / Constructor ----

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

    // ---- EnumVariantPayloadTy ----

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
        assert_eq!(range_app(&half).map(|(e, inc)| (e, inc)), Some((&int(), false)));
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
