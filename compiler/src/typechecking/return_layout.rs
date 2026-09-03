//! Direct CALL/RETURN width for known ≤2-word enum layouts.
//!
//! Scalar / niched-heap Option and heap-heap Result stay one word. Unbounded
//! `T`, PolyFn, FFI, and coroutines keep the boxed ABI at those boundaries.

use super::infer::Checker;
use super::ty::{
    is_option_ty, is_result_ty, option_inner, result_ok_err, strip_readonly, Ty, BOOL, BYTE, FLOAT,
    INT,
};

/// `Some(is_option)` when `ty` uses the two-slot `[payload, tag]` CALL/RETURN ABI.
pub fn two_word_return_kind(checker: &Checker, ty: &Ty) -> Option<bool> {
    let ty = strip_readonly(ty);
    if !ty_is_closed(ty) {
        return None;
    }
    if is_option_ty(ty) {
        if option_is_pointer_niche(checker, ty) {
            return None;
        }
        let inner = option_inner(ty)?;
        return is_immediate_word(&inner).then_some(true);
    }
    if is_result_ty(ty) {
        if result_is_pointer_niche(checker, ty) {
            return None;
        }
        let (ok, err) = result_ok_err(ty)?;
        if is_one_word_payload(checker, &ok) && is_one_word_payload(checker, &err) {
            return Some(false);
        }
        return None;
    }
    unary_user_enum_kind(checker, ty)
}

fn option_is_pointer_niche(checker: &Checker, ty: &Ty) -> bool {
    option_inner(ty).is_some_and(|inner| is_heap_word(checker, &inner))
}

fn result_is_pointer_niche(checker: &Checker, ty: &Ty) -> bool {
    result_ok_err(ty).is_some_and(|(ok, err)| {
        is_heap_word(checker, &ok) && is_heap_word(checker, &err)
    })
}

fn unary_user_enum_kind(checker: &Checker, ty: &Ty) -> Option<bool> {
    let name = enum_name(ty)?;
    if common::is_builtin_option_enum(name) || common::is_builtin_result_enum(name) {
        return None;
    }
    if common::is_builtin_ffi_enum(name) || checker.is_scalar_enum(name) || checker.is_class(name)
    {
        return None;
    }
    let vars = checker.enum_variants(name)?;
    if vars.is_empty() {
        return None;
    }
    let mut max_arity = 0usize;
    let mut any_payload = false;
    for (_, _, payload) in &vars {
        max_arity = max_arity.max(payload.len());
        if !payload.is_empty() {
            any_payload = true;
        }
        if !payload.iter().all(|p| is_one_word_payload(checker, p)) {
            return None;
        }
    }
    (max_arity == 1 && any_payload).then_some(false)
}

fn enum_name(ty: &Ty) -> Option<&str> {
    match ty {
        Ty::Con(n) | Ty::Sum { name: n, .. } => Some(n.as_str()),
        Ty::App(h, _) => match h.as_ref() {
            Ty::Con(n) => Some(n.as_str()),
            _ => None,
        },
        Ty::Constructor { owner, .. } => enum_name(owner),
        _ => None,
    }
}

fn is_immediate_word(ty: &Ty) -> bool {
    let ty = strip_readonly(ty);
    matches!(
        ty,
        Ty::Con(n) if n == INT || n == FLOAT || n == BOOL || n == BYTE
    )
}

fn is_heap_word(checker: &Checker, ty: &Ty) -> bool {
    let ty = strip_readonly(ty);
    match ty {
        Ty::Constructor { owner, .. } => is_heap_word(checker, owner),
        Ty::Con(name) => name == super::ty::STRING || checker.is_class(name),
        Ty::App(head, args) => match head.as_ref() {
            Ty::Con(name)
                if common::is_builtin_option_enum(name) || common::is_builtin_result_enum(name) =>
            {
                false
            }
            Ty::Con(name) => checker.is_class(name) && args.iter().all(ty_is_closed),
            _ => false,
        },
        Ty::List(inner) => ty_is_closed(inner),
        Ty::Tuple(items) => items.iter().all(ty_is_closed),
        Ty::Record { fields } => fields.iter().all(|(_, f)| ty_is_closed(f)),
        Ty::Array { element, .. } => ty_is_closed(element),
        Ty::Sum { name, variants }
            if !common::is_builtin_option_enum(name) && !common::is_builtin_result_enum(name) =>
        {
            variants
                .iter()
                .all(|(_, p)| p.field_types().into_iter().all(ty_is_closed))
        }
        _ => false,
    }
}

fn is_one_word_payload(checker: &Checker, ty: &Ty) -> bool {
    let ty = strip_readonly(ty);
    if !ty_is_closed(ty) {
        return false;
    }
    is_immediate_word(ty) || is_heap_word(checker, ty) || is_boxed_or_scalar_enum(checker, ty)
}

fn is_boxed_or_scalar_enum(checker: &Checker, ty: &Ty) -> bool {
    if is_option_ty(ty) || is_result_ty(ty) {
        // Nested Option/Result payloads stay one heap/niche word.
        return true;
    }
    let Some(name) = enum_name(ty) else {
        return false;
    };
    checker.is_scalar_enum(name) || checker.enum_variants(name).is_some()
}

fn ty_is_closed(ty: &Ty) -> bool {
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
    use crate::typechecking::ty::{option_ty, result_ty};

    fn checker() -> Checker {
        Checker::new()
    }

    #[test]
    fn option_int_is_two_word_option() {
        let c = checker();
        let ty = option_ty(Ty::Con(INT.into()));
        assert_eq!(two_word_return_kind(&c, &ty), Some(true));
    }

    #[test]
    fn option_string_stays_one_word_niche() {
        let c = checker();
        let ty = option_ty(Ty::Con(super::ty::STRING.into()));
        assert_eq!(two_word_return_kind(&c, &ty), None);
    }

    #[test]
    fn result_int_int_is_two_word() {
        let c = checker();
        let ty = result_ty(Ty::Con(INT.into()), Ty::Con(INT.into()));
        assert_eq!(two_word_return_kind(&c, &ty), Some(false));
    }

    #[test]
    fn result_int_string_is_two_word() {
        let c = checker();
        let ty = result_ty(Ty::Con(INT.into()), Ty::Con(super::ty::STRING.into()));
        assert_eq!(two_word_return_kind(&c, &ty), Some(false));
    }

    #[test]
    fn result_string_string_stays_one_word_niche() {
        let c = checker();
        let s = Ty::Con(super::ty::STRING.into());
        let ty = result_ty(s.clone(), s);
        assert_eq!(two_word_return_kind(&c, &ty), None);
    }

    fn unbounded_option_is_not_two_word() {
        let c = checker();
        let ty = option_ty(Ty::Var(crate::typechecking::ty::TyVarId(0)));
        assert_eq!(two_word_return_kind(&c, &ty), None);
    }
}
