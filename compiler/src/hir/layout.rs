//! The one layout query: how a value of a type crosses a word, a `CALL` and
//! a `RETURN`.
//!
//! [`of`] decides one [`Layout`] per type. The older entry points are thin
//! views of it:
//!
//! - [`crate::typechecking::value_layout::value_layout`] is the one-word
//!   encoding (boxed or a pointer niche).
//! - [`crate::typechecking::return_layout::two_word_return_enum`] is the
//!   two-slot direct `CALL`/`RETURN` width.
//! - [`crate::mir::MirLayout`] is the MIR ABI class.
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
}

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
        // Only the `unit` constructor: `Result<Tuple([]), E>` is a
        // heap-heap `NicheResult` whose `Ok` is the immortal `()` tuple.
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
    unary_user_enum_name(checker, ty).map(|name| PairKind::Enum(name.to_string()))
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
/// payload arity `<= 1`, the same shape
/// [`crate::typechecking::local_escape`] unboxes into frame slots.
fn unary_user_enum_name<'a>(checker: &Checker, ty: &'a Ty) -> Option<&'a str> {
    let name = enum_name(ty)?;
    if common::is_builtin_option_enum(name)
        || common::is_builtin_result_enum(name)
        || common::is_builtin_ffi_enum(name)
    {
        return None;
    }
    if checker.is_scalar_enum(name) || checker.is_class(name) {
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
        Ty::List(inner) => ty_is_closed(inner),
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
        Ty::List(inner) | Ty::Constructor { owner: inner, .. } => ty_is_closed(inner),
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

#[cfg(test)]
#[path = "layout.tests.rs"]
mod tests;
