//! One-word layout of `Option` / `Result` / enum values.
//!
//! [`value_layout`] is the one-word view of [`crate::hir::layout::of`]. It decides the one-word encoding of a value (boxed `ObjEnum`
//! or a pointer niche). Codegen, `HostInvoke` packing and FFI repacking all
//! ask here so producers and consumers cannot disagree. The two-word direct
//! `CALL`/`RETURN` width is [`super::return_layout::two_word_return_enum`].

use super::infer::Checker;
use super::subst::apply_ty_prune;
use super::ty::{Ty, UNIT, strip_readonly};
use crate::hir::layout::{self, Layout, is_scalar_enum_ty};
pub use crate::hir::layout::{niche_heap_only, ty_is_closed};

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
/// The one-word view of [`crate::hir::layout::of`]: a two-slot pair is boxed.
pub fn value_layout(checker: &Checker, ty: &Ty) -> ValueLayout {
    let ty = apply_ty_prune(checker.subst(), ty);
    ValueLayout::from(&layout::niche_of(checker, &ty))
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
            super::ty::INT | super::ty::FLOAT | super::ty::BOOL | super::ty::BYTE | UNIT
        )
    {
        return common::WORD_SCALAR;
    }
    if is_scalar_enum_ty(checker, ty) {
        return common::WORD_SCALAR;
    }
    if niche_heap_only(checker, ty) || value_layout(checker, ty) != ValueLayout::Boxed {
        return common::WORD_POINTER;
    }
    // A `Vec<T>` / `[T]` / `[T; N]` word is always an array object, whatever
    // `T` is (a fixed array stored as a word is materialized on the heap).
    if matches!(ty, Ty::List(_) | Ty::Array { .. })
        || matches!(ty, Ty::App(head, _) if matches!(head.as_ref(), Ty::Con(n) if n == common::BUILTIN_VEC_TYPE))
    {
        return common::WORD_POINTER;
    }
    common::WORD_UNKNOWN
}

/// `Some(elem)` when `ty` is `Vec<elem>` / `[elem]`.
pub fn vec_elem_ty(checker: &Checker, ty: &Ty) -> Option<Ty> {
    let ty = apply_ty_prune(checker.subst(), ty);
    match strip_readonly(&ty) {
        Ty::List(inner) => Some(inner.as_ref().clone()),
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
mod tests {
    use super::*;
    use crate::typechecking::ty::{int, option_ty, result_ty, string, unit};

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
        assert_eq!(kind(Ty::List(Box::new(Ty::Var(crate::typechecking::ty::TyVarId(0))))), common::WORD_POINTER);
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
