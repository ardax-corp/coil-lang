//! Two-slot CALL/RETURN width for known ≤2-word return layouts.
//!
//! `Result<int, int>`, `Result<int, heap-object>` (including unit-enum
//! errors), `Option<int>`, and user payload enums with arity ≤1 fit in
//! `[payload, tag]` without boxing an `ObjEnum`. A closed arity-2 tuple of
//! immediates (`(int, int)`, `(int, float)`, …) uses the same CALL/RETURN
//! width as `[a, b]` (second word on top) without boxing an `ObjTuple`.
//! Niched heap `Option<T>` / heap-heap `Result<T, E>` already use a
//! strictly better one-word ABI and must stay there (never
//! double-classified). Unbounded `T`, mixed heap products, and wider
//! tuples keep the boxed ABI.

use super::infer::Checker;
use super::ty::{
    is_option_ty, is_result_ty, option_inner, result_ok_err, strip_readonly, Ty, BOOL, BYTE,
    FLOAT, INT,
};

/// Kind string for a two-slot arity-2 immediate product. Not a user enum;
/// boxing uses `MakeTuple(2)` instead of the enum cascade.
pub const TWO_WORD_PRODUCT_KIND: &str = "__product2";

pub fn is_two_word_product_kind(kind: &str) -> bool {
    kind == TWO_WORD_PRODUCT_KIND
}

/// `Some(kind)` when direct `CALL`/`RETURN` of a function returning `ty`
/// can move two words instead of boxing. Enum kinds are the enum name
/// (`[payload, tag]`); [`TWO_WORD_PRODUCT_KIND`] is an arity-2 immediate
/// tuple (`[a, b]`). The kind is used when the pair must be boxed at a
/// boundary that still needs one word (`CallIndirect`, FFI, coroutines).
pub fn two_word_return_enum(checker: &Checker, ty: &Ty) -> Option<String> {
    let ty = strip_readonly(ty);
    if !ty_is_closed(ty) {
        return None;
    }
    if let Ty::Tuple(items) = ty {
        if items.len() == 2 && items.iter().all(is_immediate) {
            return Some(TWO_WORD_PRODUCT_KIND.to_string());
        }
        return None;
    }
    if is_option_ty(ty) {
        let inner = option_inner(ty)?;
        return is_immediate(&inner).then(|| common::BUILTIN_OPTION_ENUM.to_string());
    }
    if is_result_ty(ty) {
        let (ok, _err) = result_ok_err(ty)?;
        return is_immediate(&ok).then(|| common::BUILTIN_RESULT_ENUM.to_string());
    }
    unary_user_enum_name(checker, ty)
}

fn is_immediate(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::Con(n) if n == INT || n == FLOAT || n == BOOL || n == BYTE)
}

/// A closed, non-scalar, non-FFI/builtin user enum whose every variant has
/// payload arity `<= 1` — the same shape [`super::local_escape`] unboxes into
/// frame slots, generalized to a call boundary.
fn unary_user_enum_name(checker: &Checker, ty: &Ty) -> Option<String> {
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
    any_payload.then(|| name.to_string())
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

/// No unresolved type variables anywhere in `ty` (excludes unbounded generic
/// instantiations — those keep the boxed ABI, matching `CallIndirect`/PolyFn).
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
    use crate::typechecking::ty::{option_ty, result_ty, STRING};

    fn checker() -> Checker {
        Checker::new()
    }

    #[test]
    fn option_int_is_two_word() {
        let c = checker();
        let ty = option_ty(Ty::Con(INT.into()));
        assert_eq!(
            two_word_return_enum(&c, &ty),
            Some(common::BUILTIN_OPTION_ENUM.to_string())
        );
    }

    #[test]
    fn option_string_stays_one_word_niche() {
        let c = checker();
        let ty = option_ty(Ty::Con(STRING.into()));
        assert_eq!(two_word_return_enum(&c, &ty), None);
    }

    #[test]
    fn result_int_int_is_two_word() {
        let c = checker();
        let ty = result_ty(Ty::Con(INT.into()), Ty::Con(INT.into()));
        assert_eq!(
            two_word_return_enum(&c, &ty),
            Some(common::BUILTIN_RESULT_ENUM.to_string())
        );
    }

    #[test]
    fn result_int_string_is_two_word() {
        let c = checker();
        let ty = result_ty(Ty::Con(INT.into()), Ty::Con(STRING.into()));
        assert_eq!(
            two_word_return_enum(&c, &ty),
            Some(common::BUILTIN_RESULT_ENUM.to_string())
        );
    }

    #[test]
    fn result_string_string_stays_one_word_niche() {
        let c = checker();
        let s = Ty::Con(STRING.into());
        let ty = result_ty(s.clone(), s);
        assert_eq!(two_word_return_enum(&c, &ty), None);
    }

    #[test]
    fn result_string_int_stays_boxed_ok_is_not_immediate() {
        let c = checker();
        let ty = result_ty(Ty::Con(STRING.into()), Ty::Con(INT.into()));
        assert_eq!(two_word_return_enum(&c, &ty), None);
    }

    #[test]
    fn unbounded_option_is_not_two_word() {
        let c = checker();
        let ty = option_ty(Ty::Var(crate::typechecking::ty::TyVarId(0)));
        assert_eq!(two_word_return_enum(&c, &ty), None);
    }

    #[test]
    fn plain_int_is_not_two_word() {
        let c = checker();
        assert_eq!(two_word_return_enum(&c, &Ty::Con(INT.into())), None);
    }

    fn checked(src: &str) -> Checker {
        let owned = Box::leak(src.to_string().into_boxed_str());
        let ast = parser::Pratt::default().parse(owned).expect("parse");
        let mut c = Checker::new();
        let _ = c.check_program(&ast);
        assert!(c.messages().is_empty(), "{:?}", c.messages());
        c
    }

    #[test]
    fn user_payload_enum_arity_one_is_two_word_regardless_of_variant_order() {
        let src = r#"
enum Cell {
    Num(int),
    Empty,
}
fn cell(int n) -> Cell {
    return Cell::Num(n);
}
"#;
        let c = checked(src);
        assert_eq!(
            two_word_return_enum(&c, &Ty::Con("Cell".into())),
            Some("Cell".to_string())
        );

        let src_reordered = r#"
enum Cell {
    Empty,
    Num(int),
}
fn cell(int n) -> Cell {
    return Cell::Num(n);
}
"#;
        let c2 = checked(src_reordered);
        assert_eq!(
            two_word_return_enum(&c2, &Ty::Con("Cell".into())),
            Some("Cell".to_string())
        );
    }

    #[test]
    fn user_enum_with_arity_two_variant_stays_boxed() {
        let src = r#"
enum Shape {
    Rect(int, int),
    Empty,
}
fn shape() -> Shape {
    return Shape::Empty;
}
"#;
        let c = checked(src);
        assert_eq!(two_word_return_enum(&c, &Ty::Con("Shape".into())), None);
    }

    #[test]
    fn int_int_product_is_two_word() {
        let c = checker();
        let ty = Ty::Tuple(vec![Ty::Con(INT.into()), Ty::Con(INT.into())]);
        assert_eq!(
            two_word_return_enum(&c, &ty),
            Some(TWO_WORD_PRODUCT_KIND.to_string())
        );
    }

    #[test]
    fn mixed_immediate_product_is_two_word() {
        let c = checker();
        let ty = Ty::Tuple(vec![Ty::Con(INT.into()), Ty::Con(FLOAT.into())]);
        assert_eq!(
            two_word_return_enum(&c, &ty),
            Some(TWO_WORD_PRODUCT_KIND.to_string())
        );
    }

    #[test]
    fn mixed_heap_product_stays_boxed() {
        let c = checker();
        let ty = Ty::Tuple(vec![Ty::Con(INT.into()), Ty::Con(STRING.into())]);
        assert_eq!(two_word_return_enum(&c, &ty), None);
    }

    #[test]
    fn arity_three_product_stays_boxed() {
        let c = checker();
        let i = Ty::Con(INT.into());
        let ty = Ty::Tuple(vec![i.clone(), i.clone(), i]);
        assert_eq!(two_word_return_enum(&c, &ty), None);
    }
}
