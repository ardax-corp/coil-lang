//! Value representation: how a value of a static type is held in a word,
//! crosses a `CALL` / `RETURN`, and reads to the GC.
//!
//! [`of`] decides one [`Layout`] per type; every other answer here is a
//! view of it:
//!
//! - [`value_layout`] is the one-word encoding (boxed or a pointer niche),
//!   which codegen, `HostInvoke` packing and FFI repacking all ask.
//! - [`word_kind`] / [`array_elem_stamp`] are what the GC and array stamps
//!   read a word as.
//! - [`super::abi::two_word_kind`] is the two-slot direct `CALL`/`RETURN`
//!   width, and [`super::abi::MirLayout`] the MIR ABI class.
//!
//! The two-slot and niche cases are disjoint: a pair needs an immediate
//! payload, a niche a heap one.

use crate::typechecking::infer::Checker;
use crate::typechecking::subst::apply_ty_prune;
use crate::typechecking::ty::{
    BOOL, BYTE, FLOAT, INT, Ty, UNIT, is_option_ty, is_result_ty, option_inner, range_app,
    result_ok_err, strip_readonly,
};

/// How a value of some type is represented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// One word: an immediate, a heap object, or a boxed `ObjEnum`.
    Word,
    /// Two words on direct `CALL`/`RETURN`; boxed to one word elsewhere.
    Pair(PairKind),
    /// `Option<T>` with heap `T`: `0` is `None`, otherwise the payload pointer.
    NicheOption,
    /// `Result<(), E>` with heap `E`: `0` is `Ok(())`, otherwise the error pointer.
    NicheUnitResult,
    /// `Result<T, E>` with heap `T` and `E`: `Ok` is the pointer, `Err` is `pointer | 1`.
    NicheResult,
}

/// Which two-word shape a [`Layout::Pair`] is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairKind {
    /// `Option<immediate>`: `[payload, tag]`.
    Option,
    /// `Result<immediate, E>` or `Result<(), immediate>`: `[payload, tag]`.
    Result,
    /// A closed arity-2 tuple of immediates: `[a, b]`.
    Product,
    /// A numeric `Range` / `RangeInclusive`: `[start, end]`.
    Range { inclusive: bool },
    /// A closed user enum whose variants carry at most one field: `[payload, tag]`.
    Enum(String),
    /// A `readonly class` (by key) of two immediate fields: `[first, second]`
    /// (#1562). Its boxed form is an instance of the class.
    Class(String),
}

impl PairKind {
    /// The enum a `[payload, tag]` pair is a value of; `None` for a product
    /// or a range.
    pub fn enum_name(&self) -> Option<&str> {
        match self {
            Self::Option => Some(common::BUILTIN_OPTION_ENUM),
            Self::Result => Some(common::BUILTIN_RESULT_ENUM),
            Self::Enum(name) => Some(name),
            Self::Product | Self::Range { .. } | Self::Class(_) => None,
        }
    }
}

// `words` and `is_niche` only have test callers.
#[allow(dead_code)]
impl Layout {
    /// Words on a direct `CALL`/`RETURN`.
    pub fn words(&self) -> u8 {
        match self {
            Self::Pair(_) => 2,
            _ => 1,
        }
    }

    pub fn is_niche(&self) -> bool {
        matches!(
            self,
            Self::NicheOption | Self::NicheUnitResult | Self::NicheResult
        )
    }
}

/// Layout of `ty`, resolved through the checker's substitution.
#[allow(dead_code)]
pub fn of(checker: &Checker, ty: &Ty) -> Layout {
    let ty = apply_ty_prune(checker.subst(), ty);
    of_resolved(checker, &ty)
}

/// Layout of `ty` as given. An unresolved variable makes it [`Layout::Word`]
/// or a niche-free result, never a pair.
pub fn of_resolved(checker: &Checker, ty: &Ty) -> Layout {
    let ty = strip_readonly(ty);
    if ty_is_closed(ty)
        && let Some(kind) = pair_kind(checker, ty)
    {
        return Layout::Pair(kind);
    }
    niche_of(checker, ty)
}

/// The niche part of [`of_resolved`]: [`Layout::Word`] for every pair.
pub(crate) fn niche_of(checker: &Checker, ty: &Ty) -> Layout {
    let ty = strip_readonly(ty);
    if is_option_ty(ty) {
        return match option_inner(ty) {
            Some(inner) if niche_heap_only(checker, &inner) => Layout::NicheOption,
            _ => Layout::Word,
        };
    }
    if let Some((ok, err)) = result_ok_err(ty) {
        let err_heap = niche_heap_only(checker, &err);
        // `()` is the `unit` type ([`crate::typechecking::ty::tuple`]),
        // so every `Result<(), E>` with a heap `E` is this one layout.
        if matches!(strip_readonly(&ok), Ty::Con(n) if n == UNIT) && err_heap {
            return Layout::NicheUnitResult;
        }
        if err_heap && niche_heap_only(checker, &ok) {
            return Layout::NicheResult;
        }
    }
    Layout::Word
}

/// The two-word shape of a closed `ty`, if it has one.
fn pair_kind(checker: &Checker, ty: &Ty) -> Option<PairKind> {
    if let Some((elem, inclusive)) = range_app(ty) {
        return is_immediate(elem).then_some(PairKind::Range { inclusive });
    }
    if let Ty::Tuple(items) = ty {
        return (items.len() == 2 && items.iter().all(is_immediate)).then_some(PairKind::Product);
    }
    if is_option_ty(ty) {
        let inner = option_inner(ty)?;
        return is_immediate(&inner).then_some(PairKind::Option);
    }
    if is_result_ty(ty) {
        let (ok, err) = result_ok_err(ty)?;
        // `Result<(), E>` with an immediate `E` (`Result<(), int>`) would box
        // an `ObjEnum` per call; the pair is `[(), tag]`. A heap `E` keeps
        // its one-word niche.
        return (is_immediate(&ok) || (is_unit(&ok) && is_immediate(&err)))
            .then_some(PairKind::Result);
    }
    if let Some(key) = readonly_pair_class(checker, ty) {
        return Some(PairKind::Class(key));
    }
    unary_user_enum_name(checker, ty).map(|name| PairKind::Enum(name.to_string()))
}

/// The key of a `readonly class` that moves as two words like an arity-2
/// immediate tuple (#1562): not generic, no `drop`, exactly two fields,
/// each `int`, `float`, `bool` or `byte`. Two instances with the same
/// fields cannot be told apart (structural `==`, no `weak()`), so the
/// object need not exist until something takes it as one word.
pub(crate) fn readonly_pair_class(checker: &Checker, ty: &Ty) -> Option<String> {
    let Ty::Con(name) = strip_readonly(ty) else {
        return None;
    };
    let key = checker.resolve_class_key(name)?;
    if !checker.is_readonly_class(&key) || checker.class_has_drop(&key) || checker.generics().generic_type_ctors.contains_key(&key) {
        return None;
    }
    let fields = checker.class_fields(&key)?;
    (fields.len() == 2 && fields.iter().all(|(_, t)| is_immediate(t))).then_some(key)
}

/// `int`, `float`, `bool` or `byte`.
pub(crate) fn is_immediate(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::Con(n) if n == INT || n == FLOAT || n == BOOL || n == BYTE)
}

/// `()`: the `unit` constructor or the empty tuple.
pub(crate) fn is_unit(ty: &Ty) -> bool {
    match strip_readonly(ty) {
        Ty::Con(n) => n == UNIT,
        Ty::Tuple(items) => items.is_empty(),
        _ => false,
    }
}

/// A closed, non-scalar, non-FFI/builtin user enum whose every variant has
/// payload arity `<= 1`, the shape frame-slot unboxing uses.
fn unary_user_enum_name<'a>(checker: &Checker, ty: &'a Ty) -> Option<&'a str> {
    let name = enum_name(ty)?;
    if common::is_builtin_option_enum(name)
        || common::is_builtin_result_enum(name)
        || common::is_builtin_ffi_enum(name)
    {
        return None;
    }
    // A drop enum's payload variants are heap objects carrying a
    // finalizer (`TagEnumType`), so it stays boxed. Other `impl` blocks do
    // not change the shape.
    if checker.is_scalar_enum(name) || checker.enum_has_drop(name) {
        return None;
    }
    let vars = checker.enum_variants(name)?;
    if vars.is_empty() {
        return None;
    }
    let mut any_payload = false;
    for (_, _, payload) in &vars {
        if payload.len() > 1 {
            return None;
        }
        if let Some(p) = payload.first() {
            if !ty_is_closed(p) {
                return None;
            }
            any_payload = true;
        }
    }
    any_payload.then_some(name)
}

fn enum_name(ty: &Ty) -> Option<&str> {
    match ty {
        Ty::Con(n) | Ty::Sum { name: n, .. } => Some(n.as_str()),
        Ty::App(head, _) => match head.as_ref() {
            Ty::Con(n) => Some(n.as_str()),
            _ => None,
        },
        Ty::Constructor { owner, .. } => enum_name(owner),
        _ => None,
    }
}

/// A scalar-backed enum (`#[repr(int)]`), named or as a variant /
/// sum type (`Level::High` types as `Constructor { owner: Sum }`).
pub(crate) fn is_scalar_enum_ty(checker: &Checker, ty: &Ty) -> bool {
    match strip_readonly(ty) {
        Ty::Con(name) | Ty::Sum { name, .. } => checker.is_scalar_enum(name),
        Ty::Constructor { owner, .. } => is_scalar_enum_ty(checker, owner),
        _ => false,
    }
}

/// The enum `ty` names when it is scalar-backed ([`is_scalar_enum_ty`]).
pub(crate) fn scalar_enum_name<'t>(checker: &Checker, ty: &'t Ty) -> Option<&'t str> {
    is_scalar_enum_ty(checker, ty).then(|| enum_name(strip_readonly(ty))).flatten()
}

/// True when `ty` is a ground heap object, so a niche can use `0` / bit 0.
pub fn niche_heap_only(checker: &Checker, ty: &Ty) -> bool {
    let ty = strip_readonly(ty);
    if is_scalar_enum_ty(checker, ty) {
        return false;
    }
    match ty {
        Ty::Constructor { owner, .. } => niche_heap_only(checker, owner),
        Ty::Con(name) => {
            if name == "string" || checker.is_class(name) {
                true
            } else if common::is_builtin_io_error_enum(name)
                || common::is_builtin_thread_error_enum(name)
                || common::is_builtin_env_error_enum(name)
            {
                // Virtual unit-error enums stay heap even if this file
                // never imported the tags (per-file `check_program` reset).
                true
            } else if common::is_builtin_option_enum(name)
                || common::is_builtin_result_enum(name)
                || checker.is_scalar_enum(name)
            {
                false
            } else {
                // Unit / closed user enums are heap `ObjEnum` (IoError, …).
                checker.enum_variants(name).is_some_and(|vars| {
                    !vars.is_empty()
                        && vars
                            .iter()
                            .all(|(_, _, payload)| payload.iter().all(ty_is_closed))
                })
            }
        }
        Ty::App(head, args) => {
            let Ty::Con(name) = head.as_ref() else {
                return false;
            };
            if common::is_builtin_option_enum(name) || common::is_builtin_result_enum(name) {
                return false;
            }
            checker.is_class(name) && args.iter().all(ty_is_closed)
        }
        Ty::Sum { name, .. }
            if common::is_builtin_option_enum(name) || common::is_builtin_result_enum(name) =>
        {
            false
        }
        Ty::Tuple(items) => items.iter().all(ty_is_closed),
        Ty::Record { fields } => fields.iter().all(|(_, field)| ty_is_closed(field)),
        Ty::Sum { variants, .. } => variants
            .iter()
            .all(|(_, payload)| payload.field_types().into_iter().all(ty_is_closed)),
        _ => false,
    }
}

/// No unresolved type variables anywhere in `ty`.
pub fn ty_is_closed(ty: &Ty) -> bool {
    let ty = strip_readonly(ty);
    match ty {
        Ty::Var(_) | Ty::Fun(_, _) | Ty::Existential { .. } | Ty::Forall { .. } => false,
        Ty::Constructor { owner: inner, .. } => ty_is_closed(inner),
        Ty::App(_, args) => args.iter().all(ty_is_closed),
        Ty::Tuple(items) => items.iter().all(ty_is_closed),
        Ty::Record { fields } => fields.iter().all(|(_, f)| ty_is_closed(f)),
        Ty::Array { element, .. } => ty_is_closed(element),
        Ty::Sum { variants, .. } => variants
            .iter()
            .all(|(_, p)| p.field_types().into_iter().all(ty_is_closed)),
        Ty::Con(_) | Ty::Never => true,
        Ty::Readonly(_) => unreachable!("stripped"),
    }
}

/// `ty` with each named type parameter in `params` replaced by the matching
/// type in `args` (a generic declaration's field at one instance).
pub fn bind_params(ty: &Ty, params: &[String], args: &[Ty]) -> Ty {
    let bind = |t: &Ty| bind_params(t, params, args);
    match ty {
        Ty::Con(name) => match params.iter().position(|p| p == name) {
            Some(i) => args.get(i).cloned().unwrap_or_else(|| ty.clone()),
            None => ty.clone(),
        },
        Ty::App(head, items) => Ty::App(Box::new(bind(head)), items.iter().map(bind).collect()),
        Ty::Fun(a, b) => Ty::Fun(Box::new(bind(a)), Box::new(bind(b))),
        Ty::Readonly(inner) => Ty::Readonly(Box::new(bind(inner))),
        Ty::Tuple(items) => Ty::Tuple(items.iter().map(bind).collect()),
        Ty::Array { element, length } => Ty::Array {
            element: Box::new(bind(element)),
            length: *length,
        },
        Ty::Record { fields } => Ty::Record {
            fields: fields.iter().map(|(n, f)| (n.clone(), bind(f))).collect(),
        },
        _ => ty.clone(),
    }
}

/// One-word representation of a value of some type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueLayout {
    /// Immediate, heap object, or boxed `ObjEnum` — no special encoding.
    Boxed,
    /// `Option<T>` with heap `T`: `0` is `None`, otherwise the payload pointer.
    NicheOption,
    /// `Result<(), E>` with heap `E`: `0` is `Ok(())`, otherwise the error pointer.
    NicheUnitResult,
    /// `Result<T, E>` with heap `T` and `E`: `Ok` is the pointer, `Err` is `pointer | 1`.
    NicheResult,
}

impl ValueLayout {
    pub fn is_niche_option(&self) -> bool {
        matches!(self, Self::NicheOption)
    }

    pub fn is_niche_unit_result(&self) -> bool {
        matches!(self, Self::NicheUnitResult)
    }

    pub fn is_niche_result(&self) -> bool {
        matches!(self, Self::NicheResult)
    }

    /// `HostInvoke` layout bits a host native packs this value with.
    pub fn host_enum_layout(&self) -> u32 {
        match self {
            Self::NicheOption | Self::NicheUnitResult => common::HOST_ENUM_LAYOUT_OPTION_NICHE,
            Self::NicheResult => common::HOST_ENUM_LAYOUT_RESULT_NICHE,
            Self::Boxed => common::HOST_ENUM_LAYOUT_BOXED,
        }
    }
}

/// Layout of a value of type `ty` (resolved through the checker's substitution).
/// The one-word view of [`crate::layout::of`]: a two-slot pair is boxed.
pub fn value_layout(checker: &Checker, ty: &Ty) -> ValueLayout {
    let ty = apply_ty_prune(checker.subst(), ty);
    ValueLayout::from(&niche_of(checker, &ty))
}

impl From<&Layout> for ValueLayout {
    fn from(layout: &Layout) -> Self {
        match layout {
            Layout::NicheOption => Self::NicheOption,
            Layout::NicheUnitResult => Self::NicheUnitResult,
            Layout::NicheResult => Self::NicheResult,
            Layout::Word | Layout::Pair(_) => Self::Boxed,
        }
    }
}

/// How a heap word of static type `ty` reads (`common::WORD_*`): a number /
/// bool / scalar enum is never a reference; a ground heap object or a niche
/// word is `0` or an object address (bit 0 set for a `Result` `Err`);
/// anything generic or unresolved stays unknown.
pub fn word_kind(checker: &Checker, ty: &Ty) -> u8 {
    let ty = apply_ty_prune(checker.subst(), ty);
    let ty = strip_readonly(&ty);
    if let Ty::Con(name) = ty
        && matches!(
            name.as_str(),
            crate::typechecking::ty::INT | crate::typechecking::ty::FLOAT | crate::typechecking::ty::BOOL | crate::typechecking::ty::BYTE | UNIT
        )
    {
        return common::WORD_SCALAR;
    }
    if is_scalar_enum_ty(checker, ty) {
        // A `#[repr(string)]` value is its backing string's heap word.
        let string = scalar_enum_name(checker, ty)
            .and_then(|name| checker.scalar_value_ty(name))
            .is_some_and(|b| matches!(strip_readonly(&b), Ty::Con(n) if n == crate::typechecking::ty::STRING));
        return if string { common::WORD_POINTER } else { common::WORD_SCALAR };
    }
    if niche_heap_only(checker, ty) || value_layout(checker, ty) != ValueLayout::Boxed {
        return common::WORD_POINTER;
    }
    // A `Vec<T>` / `[T]` / `[T; N]` word is always an array object, whatever
    // `T` is (a fixed array stored as a word is materialized on the heap).
    if matches!(ty, Ty::Array { .. })
        || matches!(ty, Ty::App(head, _) if matches!(head.as_ref(), Ty::Con(n) if n == common::BUILTIN_VEC_TYPE))
    {
        return common::WORD_POINTER;
    }
    common::WORD_UNKNOWN
}

/// Element kind an array of `elem` is stamped with at construction
/// (`MakeArrayK`): `WORD_POINTER` for ground heap words,
/// `WORD_BYTE` for `byte` / `bool` (the runtime packs those one byte per
/// element), else `WORD_UNKNOWN` (no stamp).
pub fn array_elem_stamp(checker: &Checker, elem: &Ty) -> u8 {
    let ty = apply_ty_prune(checker.subst(), elem);
    if let Ty::Con(name) = strip_readonly(&ty)
        && matches!(name.as_str(), crate::typechecking::ty::BOOL | crate::typechecking::ty::BYTE)
    {
        return common::WORD_BYTE;
    }
    match word_kind(checker, &ty) {
        common::WORD_POINTER => common::WORD_POINTER,
        _ => common::WORD_UNKNOWN,
    }
}

/// `Some(elem)` when `ty` is `Vec<elem>` / `[elem]`.
pub fn vec_elem_ty(checker: &Checker, ty: &Ty) -> Option<Ty> {
    let ty = apply_ty_prune(checker.subst(), ty);
    match strip_readonly(&ty) {
        Ty::App(head, args)
            if args.len() == 1
                && matches!(head.as_ref(), Ty::Con(n) if n == common::BUILTIN_VEC_TYPE) =>
        {
            Some(args[0].clone())
        }
        _ => None,
    }
}

#[cfg(test)]
mod word_tests {
    use super::*;
    use crate::typechecking::ty::{array, int, option_ty, result_ty, string, unit};

    #[test]
    fn layouts_follow_payload_heapness() {
        let c = Checker::new();
        let layout = |ty: Ty| value_layout(&c, &ty);
        assert_eq!(layout(option_ty(string())), ValueLayout::NicheOption);
        assert_eq!(layout(option_ty(int())), ValueLayout::Boxed);
        assert_eq!(
            layout(result_ty(unit(), string())),
            ValueLayout::NicheUnitResult
        );
        assert_eq!(
            layout(result_ty(string(), string())),
            ValueLayout::NicheResult
        );
        assert_eq!(layout(result_ty(int(), string())), ValueLayout::Boxed);
        assert_eq!(layout(string()), ValueLayout::Boxed);
    }

    #[test]
    fn word_kinds_follow_static_types() {
        let c = Checker::new();
        let kind = |ty: Ty| word_kind(&c, &ty);
        assert_eq!(kind(int()), common::WORD_SCALAR);
        assert_eq!(kind(Ty::Con("float".into())), common::WORD_SCALAR);
        assert_eq!(kind(Ty::Con("bool".into())), common::WORD_SCALAR);
        assert_eq!(kind(unit()), common::WORD_SCALAR);
        assert_eq!(kind(string()), common::WORD_POINTER);
        assert_eq!(kind(option_ty(string())), common::WORD_POINTER);
        assert_eq!(kind(result_ty(string(), string())), common::WORD_POINTER);
        // Boxed `Option<int>` and unresolved generics stay unknown.
        assert_eq!(kind(option_ty(int())), common::WORD_UNKNOWN);
        assert_eq!(kind(Ty::Con("T".into())), common::WORD_UNKNOWN);
        // Arrays are objects whatever their element type.
        assert_eq!(kind(array(Ty::Var(crate::typechecking::ty::TyVarId(0)))), common::WORD_POINTER);
    }

    #[test]
    fn array_stamps_pack_bytes_and_bools_only() {
        let c = Checker::new();
        let stamp = |ty: Ty| array_elem_stamp(&c, &ty);
        assert_eq!(stamp(Ty::Con("byte".into())), common::WORD_BYTE);
        assert_eq!(stamp(Ty::Con("bool".into())), common::WORD_BYTE);
        assert_eq!(stamp(int()), common::WORD_UNKNOWN);
        assert_eq!(stamp(Ty::Con("float".into())), common::WORD_UNKNOWN);
        assert_eq!(stamp(string()), common::WORD_POINTER);
        assert_eq!(stamp(option_ty(int())), common::WORD_UNKNOWN);
        assert_eq!(stamp(Ty::Con("T".into())), common::WORD_UNKNOWN);
    }

    #[test]
    fn host_bits_match_layout() {
        assert_eq!(
            ValueLayout::NicheUnitResult.host_enum_layout(),
            common::HOST_ENUM_LAYOUT_OPTION_NICHE
        );
        assert_eq!(
            ValueLayout::NicheResult.host_enum_layout(),
            common::HOST_ENUM_LAYOUT_RESULT_NICHE
        );
        assert_eq!(
            ValueLayout::Boxed.host_enum_layout(),
            common::HOST_ENUM_LAYOUT_BOXED
        );
    }
}

#[cfg(test)]
#[path = "rep.tests.rs"]
mod tests;
