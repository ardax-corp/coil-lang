//! Two-slot CALL/RETURN width for known ≤2-word return layouts.
//!
//! `Result<int, int>`, `Result<int, heap-object>` (including unit-enum
//! errors), `Result<(), int>` (payload word `0`), `Option<int>`, and user
//! payload enums with arity ≤1 fit in
//! `[payload, tag]` without boxing an `ObjEnum`. A closed arity-2 tuple of
//! immediates (`(int, int)`, `(int, float)`, …) uses the same CALL/RETURN
//! width as `[a, b]` (second word on top) without boxing an `ObjTuple`.
//! Niched heap `Option<T>` / heap-heap `Result<T, E>` already use a
//! strictly better one-word ABI and must stay there (never
//! double-classified). Unbounded `T`, mixed heap products, and wider
//! tuples keep the boxed ABI.

use super::infer::Checker;
use super::ty::{Ty, range_app};
use crate::hir::layout::{self, Layout, PairKind, is_immediate};

/// Heap `Range` `InitTyped` id. High so user class ids (from 1) stay put.
pub const RANGE_HEAP_TYPE_ID: u32 = common::RANGE_HEAP_TYPE_ID;

/// Heap `RangeInclusive` `InitTyped` id. Inclusive lives in the type, not a slot.
pub const RANGE_INCLUSIVE_HEAP_TYPE_ID: u32 = common::RANGE_INCLUSIVE_HEAP_TYPE_ID;

/// Slotted heap object for an escaped numeric range (`start` = 0, `end` = 1).
pub fn range_heap_type_id(inclusive: bool) -> u32 {
    if inclusive {
        RANGE_INCLUSIVE_HEAP_TYPE_ID
    } else {
        RANGE_HEAP_TYPE_ID
    }
}

/// `Some(PairKind::Range { .. })` when `ty` is a numeric `Range` /
/// `RangeInclusive` two-slot pair.
pub fn two_word_range_kind(ty: &Ty) -> Option<PairKind> {
    let (elem, inclusive) = range_app(ty)?;
    is_immediate(elem).then_some(PairKind::Range { inclusive })
}

/// `Some(kind)` when direct `CALL`/`RETURN` of a function returning `ty`
/// can move two words instead of boxing: `[payload, tag]` of an enum,
/// `[a, b]` of an arity-2 immediate tuple, `[start, end]` of a numeric
/// range. The kind is used when the pair must be boxed at a boundary that
/// still needs one word (`CallIndirect`, FFI, coroutines).
/// The two-slot view of [`crate::hir::layout::of_resolved`].
pub fn two_word_kind(checker: &Checker, ty: &Ty) -> Option<PairKind> {
    match layout::of_resolved(checker, ty) {
        Layout::Pair(kind) => Some(kind),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typechecking::ty::{FLOAT, INT, STRING, option_ty, range_inclusive_ty, range_ty, result_ty};

    fn checker() -> Checker {
        Checker::new()
    }

    #[test]
    fn option_int_is_two_word() {
        let c = checker();
        let ty = option_ty(Ty::Con(INT.into()));
        assert_eq!(
            two_word_kind(&c, &ty),
            Some(PairKind::Option)
        );
    }

    #[test]
    fn option_string_stays_one_word_niche() {
        let c = checker();
        let ty = option_ty(Ty::Con(STRING.into()));
        assert_eq!(two_word_kind(&c, &ty), None);
    }

    #[test]
    fn result_int_int_is_two_word() {
        let c = checker();
        let ty = result_ty(Ty::Con(INT.into()), Ty::Con(INT.into()));
        assert_eq!(
            two_word_kind(&c, &ty),
            Some(PairKind::Result)
        );
    }

    #[test]
    fn result_int_string_is_two_word() {
        let c = checker();
        let ty = result_ty(Ty::Con(INT.into()), Ty::Con(STRING.into()));
        assert_eq!(
            two_word_kind(&c, &ty),
            Some(PairKind::Result)
        );
    }

    #[test]
    fn result_unit_int_is_two_word() {
        let c = checker();
        for unit in [Ty::Con(crate::typechecking::ty::UNIT.into()), Ty::Tuple(vec![])] {
            let ty = result_ty(unit, Ty::Con(INT.into()));
            assert_eq!(
                two_word_kind(&c, &ty),
                Some(PairKind::Result)
            );
        }
    }

    #[test]
    fn result_unit_string_stays_one_word_niche() {
        let c = checker();
        let ty = result_ty(Ty::Tuple(vec![]), Ty::Con(STRING.into()));
        assert_eq!(two_word_kind(&c, &ty), None);
    }

    #[test]
    fn result_string_string_stays_one_word_niche() {
        let c = checker();
        let s = Ty::Con(STRING.into());
        let ty = result_ty(s.clone(), s);
        assert_eq!(two_word_kind(&c, &ty), None);
    }

    #[test]
    fn result_string_int_stays_boxed_ok_is_not_immediate() {
        let c = checker();
        let ty = result_ty(Ty::Con(STRING.into()), Ty::Con(INT.into()));
        assert_eq!(two_word_kind(&c, &ty), None);
    }

    #[test]
    fn unbounded_option_is_not_two_word() {
        let c = checker();
        let ty = option_ty(Ty::Var(crate::typechecking::ty::TyVarId(0)));
        assert_eq!(two_word_kind(&c, &ty), None);
    }

    #[test]
    fn plain_int_is_not_two_word() {
        let c = checker();
        assert_eq!(two_word_kind(&c, &Ty::Con(INT.into())), None);
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
            two_word_kind(&c, &Ty::Con("Cell".into())),
            Some(PairKind::Enum("Cell".to_string()))
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
            two_word_kind(&c2, &Ty::Con("Cell".into())),
            Some(PairKind::Enum("Cell".to_string()))
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
        assert_eq!(two_word_kind(&c, &Ty::Con("Shape".into())), None);
    }

    #[test]
    fn int_int_product_is_two_word() {
        let c = checker();
        let ty = Ty::Tuple(vec![Ty::Con(INT.into()), Ty::Con(INT.into())]);
        assert_eq!(
            two_word_kind(&c, &ty),
            Some(PairKind::Product)
        );
    }

    #[test]
    fn mixed_immediate_product_is_two_word() {
        let c = checker();
        let ty = Ty::Tuple(vec![Ty::Con(INT.into()), Ty::Con(FLOAT.into())]);
        assert_eq!(
            two_word_kind(&c, &ty),
            Some(PairKind::Product)
        );
    }

    #[test]
    fn mixed_heap_product_stays_boxed() {
        let c = checker();
        let ty = Ty::Tuple(vec![Ty::Con(INT.into()), Ty::Con(STRING.into())]);
        assert_eq!(two_word_kind(&c, &ty), None);
    }

    #[test]
    fn arity_three_product_stays_boxed() {
        let c = checker();
        let i = Ty::Con(INT.into());
        let ty = Ty::Tuple(vec![i.clone(), i.clone(), i]);
        assert_eq!(two_word_kind(&c, &ty), None);
    }

    #[test]
    fn range_heap_type_ids_differ() {
        assert_ne!(range_heap_type_id(false), 0);
        assert_ne!(range_heap_type_id(true), 0);
        assert_ne!(range_heap_type_id(false), range_heap_type_id(true));
    }

    #[test]
    fn numeric_range_is_two_word() {
        let c = checker();
        assert_eq!(
            two_word_kind(&c, &range_ty(Ty::Con(INT.into()))),
            Some(PairKind::Range { inclusive: false })
        );
        assert_eq!(
            two_word_kind(&c, &range_inclusive_ty(Ty::Con(FLOAT.into()))),
            Some(PairKind::Range { inclusive: true })
        );
    }

    #[test]
    fn string_range_stays_boxed() {
        let c = checker();
        assert_eq!(
            two_word_kind(&c, &range_ty(Ty::Con(STRING.into()))),
            None
        );
    }
}
