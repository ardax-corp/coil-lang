use super::*;
use crate::mir::MirLayout;
use crate::typechecking::return_layout::two_word_return_enum;
use crate::typechecking::ty::{TyVarId, int, option_ty, range_ty, result_ty, string, unit};
use crate::typechecking::value_layout::{ValueLayout, value_layout};

fn checked(src: &str) -> Checker {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut c = Checker::new();
    let _ = c.check_program(&ast);
    assert!(c.messages().is_empty(), "{:?}", c.messages());
    c
}

const SRC: &str = r#"
enum Cell {
    Num(int),
    Empty,
}
enum Shape {
    Rect(int, int),
    Empty,
}
#[repr(int)]
enum Level {
    Low = 1,
    High = 2,
}
class Point {
    pub x: int,
}
fn cell(int n) -> Cell {
    return Cell::Num(n);
}
"#;

fn con(name: &str) -> Ty {
    Ty::Con(name.into())
}

fn empty_tuple() -> Ty {
    Ty::Tuple(vec![])
}

#[test]
fn one_layout_per_type() {
    let c = checked(SRC);
    let cases: Vec<(Ty, Layout)> = vec![
        (int(), Layout::Word),
        (string(), Layout::Word),
        (option_ty(int()), Layout::Pair(PairKind::Option)),
        (option_ty(string()), Layout::NicheOption),
        (option_ty(con("Point")), Layout::NicheOption),
        (option_ty(con("Cell")), Layout::NicheOption),
        (option_ty(con("Level")), Layout::Word),
        (option_ty(option_ty(int())), Layout::Word),
        (option_ty(Ty::Var(TyVarId(0))), Layout::Word),
        (result_ty(int(), int()), Layout::Pair(PairKind::Result)),
        (result_ty(int(), string()), Layout::Pair(PairKind::Result)),
        (result_ty(unit(), int()), Layout::Pair(PairKind::Result)),
        (result_ty(empty_tuple(), int()), Layout::Pair(PairKind::Result)),
        (result_ty(unit(), string()), Layout::NicheUnitResult),
        // `()` spelled as the empty tuple is a heap object, so this is the
        // heap-heap niche with the immortal `()` as its `Ok`.
        (result_ty(empty_tuple(), string()), Layout::NicheResult),
        (result_ty(string(), string()), Layout::NicheResult),
        (result_ty(string(), int()), Layout::Word),
        (Ty::Tuple(vec![int(), int()]), Layout::Pair(PairKind::Product)),
        (Ty::Tuple(vec![int(), string()]), Layout::Word),
        (Ty::Tuple(vec![int(), int(), int()]), Layout::Word),
        (range_ty(int()), Layout::Pair(PairKind::Range { inclusive: false })),
        (range_ty(string()), Layout::Word),
        (con("Cell"), Layout::Pair(PairKind::Enum("Cell".into()))),
        (con("Shape"), Layout::Word),
        (con("Level"), Layout::Word),
        (con("Point"), Layout::Word),
    ];
    for (ty, want) in cases {
        assert_eq!(of(&c, &ty), want, "{ty:?}");
    }
}

/// The three older entry points are views of one answer, so they agree.
#[test]
fn views_agree_with_the_layout() {
    let c = checked(SRC);
    let tys = [
        option_ty(int()),
        option_ty(string()),
        option_ty(con("Level")),
        result_ty(unit(), int()),
        result_ty(unit(), string()),
        result_ty(empty_tuple(), string()),
        result_ty(string(), string()),
        result_ty(string(), int()),
        Ty::Tuple(vec![int(), int()]),
        con("Cell"),
        con("Shape"),
    ];
    for ty in tys {
        let layout = of(&c, &ty);
        assert_eq!(
            two_word_return_enum(&c, &ty).is_some(),
            layout.words() == 2,
            "{ty:?}"
        );
        assert_eq!(
            value_layout(&c, &ty) != ValueLayout::Boxed,
            layout.is_niche(),
            "{ty:?}"
        );
        assert_eq!(MirLayout::from_coil_ty(&c, &ty), MirLayout::from(&layout), "{ty:?}");
    }
}

#[test]
fn pair_kinds_keep_their_names() {
    use crate::typechecking::return_layout::{TWO_WORD_PRODUCT_KIND, TWO_WORD_RANGE_KIND};
    let c = checked(SRC);
    let name = |ty: Ty| two_word_return_enum(&c, &ty);
    assert_eq!(name(option_ty(int())).as_deref(), Some(common::BUILTIN_OPTION_ENUM));
    assert_eq!(name(result_ty(int(), int())).as_deref(), Some(common::BUILTIN_RESULT_ENUM));
    assert_eq!(name(Ty::Tuple(vec![int(), int()])).as_deref(), Some(TWO_WORD_PRODUCT_KIND));
    assert_eq!(name(range_ty(int())).as_deref(), Some(TWO_WORD_RANGE_KIND));
    assert_eq!(name(con("Cell")).as_deref(), Some("Cell"));
}
