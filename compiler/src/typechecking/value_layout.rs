//! One layout query for `Option` / `Result` / enum values.
//!
//! [`value_layout`] decides the one-word encoding of a value (boxed `ObjEnum`
//! or a pointer niche). Codegen, `HostInvoke` packing and FFI repacking all
//! ask here so producers and consumers cannot disagree. The two-word direct
//! `CALL`/`RETURN` width is [`super::return_layout::two_word_return_enum`].

use super::infer::Checker;
use super::subst::apply_ty_prune;
use super::ty::{Ty, UNIT, is_option_ty, option_inner, result_ok_err, strip_readonly};

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
pub fn value_layout(checker: &Checker, ty: &Ty) -> ValueLayout {
    let ty = apply_ty_prune(checker.subst(), ty);
    let ty = strip_readonly(&ty);
    if is_option_ty(ty) {
        return match option_inner(ty) {
            Some(inner) if niche_heap_only(checker, &inner) => ValueLayout::NicheOption,
            _ => ValueLayout::Boxed,
        };
    }
    if let Some((ok, err)) = result_ok_err(ty) {
        let err_heap = niche_heap_only(checker, &err);
        if matches!(strip_readonly(&ok), Ty::Con(n) if n == UNIT) && err_heap {
            return ValueLayout::NicheUnitResult;
        }
        if err_heap && niche_heap_only(checker, &ok) {
            return ValueLayout::NicheResult;
        }
    }
    ValueLayout::Boxed
}

/// True when `ty` is a ground heap object, so a niche can use `0` / bit 0.
pub fn niche_heap_only(checker: &Checker, ty: &Ty) -> bool {
    let ty = strip_readonly(ty);
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
