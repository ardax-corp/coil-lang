//! Which rewrites the fold makes.

use super::*;
use crate::hir::build_module;
use crate::typechecking::infer::Checker;

/// How many assignments `f` keeps after `fold`.
fn assigns_left(src: &str) -> usize {
    kinds_left(src, |k| matches!(k, HirKind::Assign { .. }))
}

/// How many of `f`'s nodes `keep` holds for after `fold`.
fn kinds_left(src: &str, keep: fn(&HirKind) -> bool) -> usize {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let body = module.bodies.into_iter().find(|b| b.name == "f").expect("body");
    let out = fold(&body).unwrap_or(body);
    let mut n = 0;
    let mut stack = vec![out.root.expect("root")];
    while let Some(id) = stack.pop() {
        n += usize::from(keep(&out.expr(id).kind));
        stack.extend(children(&out, id));
    }
    n
}

#[test]
fn a_self_assignment_goes() {
    assert_eq!(assigns_left("fn f(int x) -> int { let y = x; y = y; return y; }"), 0);
    // `y = y + 0` folds to `y = y`, which then goes.
    assert_eq!(assigns_left("fn f(int x) -> int { let y = x; y = y + 0; return y; }"), 0);
    assert_eq!(assigns_left("fn f(int x) -> int { let y = x; y = x; return y; }"), 1);
}

/// How many `match`es `f` keeps after `fold`.
fn matches_left(src: &str) -> usize {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let body = module.bodies.into_iter().find(|b| b.name == "f").expect("body");
    let out = fold(&body).unwrap_or(body);
    let mut n = 0;
    let mut stack = vec![out.root.expect("root")];
    while let Some(id) = stack.pop() {
        n += usize::from(matches!(out.expr(id).kind, HirKind::Match { .. }));
        stack.extend(children(&out, id));
    }
    n
}

#[test]
fn a_match_on_a_constructor_takes_its_arm() {
    let m = |arms: &str| matches_left(&format!("fn f(int i) -> int {{ return match Option::Some(i) {{ {arms} }}; }}"));
    assert_eq!(m("Option::Some(x) => x, Option::None => 0,"), 0);
    assert_eq!(m("Option::None => 0, Option::Some(x) => x + 1,"), 0);
    assert_eq!(m("Option::None => 0, default => 1,"), 0);
    // A nested pattern or a binding of the whole value stays.
    assert_eq!(m("Option::Some(3) => 1, default => 0,"), 1);
    assert_eq!(m("v => 1,"), 1);
}

/// `f`'s returned expression after `fold`, printed.
fn folded_return(src: &str) -> String {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let body = module.bodies.into_iter().find(|b| b.name == "f").expect("body");
    let out = fold(&body).unwrap_or(body);
    let mut stack = vec![out.root.expect("root")];
    while let Some(id) = stack.pop() {
        if let HirKind::Return(Some(v)) = out.expr(id).kind {
            return match &out.expr(v).kind {
                HirKind::Bin { op, lhs, rhs } => {
                    let side = |k: HirId| if matches!(out.expr(k).kind, HirKind::Lit(_)) { "lit" } else { "x" };
                    format!("{} {op:?} {}", side(*lhs), side(*rhs))
                }
                other => format!("{other:?}"),
            };
        }
        stack.extend(children(&out, id));
    }
    panic!("no return");
}

#[test]
fn an_int_literal_moves_to_the_right() {
    assert_eq!(folded_return("fn f(int x) -> int { return 3 * x; }"), "x IntMul lit");
    assert_eq!(folded_return("fn f(int x) -> bool { return 1 < x; }"), "x Gt lit");
    assert_eq!(folded_return("fn f(int x) -> bool { return 1 >= x; }"), "x Le lit");
    // Not commutative.
    assert_eq!(folded_return("fn f(int x) -> int { return 3 - x; }"), "lit IntSub x");
}

#[test]
fn int_literals_fold_to_any_64_bit_result() {
    let f = |e: &str| folded_return(&format!("fn f() -> int {{ return {e}; }}"));
    assert_eq!(f("2147483647 + 1"), "Lit(Int(2147483648))");
    assert_eq!(f("4000000 * 4000000"), "Lit(Int(16000000000000))");
    assert_eq!(f("1 << 63"), "Lit(Int(-9223372036854775808))");
    assert_eq!(f("2 ** 62"), "Lit(Int(4611686018427387904))");
    assert_eq!(f("9223372036854775806 + 1"), "Lit(Int(9223372036854775807))");
    // An op that would trap stays, so it traps at run time.
    assert_eq!(f("9223372036854775807 + 1"), "lit IntAdd lit");
    assert_eq!(f("4294967296 * 4294967296"), "lit IntMul lit");
    assert_eq!(f("2 ** 63"), "lit IntPow lit");
    assert_eq!(f("1 << 64"), "lit Shl lit");
    assert_eq!(f("1 / 0"), "lit IntDiv lit");
}

#[test]
fn stores_to_a_local_nothing_reads_go() {
    // `j` is only ever stored; `y` is read.
    assert_eq!(assigns_left("fn f(int x) -> int { let j = 0; j = x; j = 3; let y = x; y = 4; return y; }"), 1);
    // A store of something with an effect keeps only the effect (#1831).
    let src = "fn g() -> int { return 1; } fn f(int x) -> int { let j = 0; j = g(); return x; }";
    assert_eq!(assigns_left(src), 0);
    assert_eq!(kinds_left(src, |k| matches!(k, HirKind::Call { .. })), 1);
}

#[test]
fn a_local_bound_once_to_a_literal_folds_into_its_reads() {
    // `m` is never written again, so its reads are the literal and the
    // arithmetic and comparisons on it fold in turn.
    assert_eq!(
        folded_return("fn f() -> int { let m = 1; let b = m * 2 + 1; if b > 100 { return 0; } return b; }"),
        "Lit(Int(3))"
    );
    // A `let` that folds to a literal reads as one in the next round.
    assert_eq!(folded_return("fn f() -> int { let m = 1; let k = m + 1; let j = k * 3; return j; }"), "Lit(Int(6))");
    // A local written again keeps its reads.
    assert_eq!(folded_return("fn f(int x) -> int { let m = 1; if x > 0 { m = 2; } return m * 2; }"), "x IntMul lit");
}

#[test]
fn a_match_on_a_local_bound_to_a_constructor_takes_its_arm() {
    // An inlined callee's parameter is a `let` of the caller's constructor.
    let m = |src: &str| matches_left(src);
    assert_eq!(m("fn f(int i) -> int { let o = Option::Some(i); return match o { Option::Some(x) => x, Option::None => 0, }; }"), 0);
    // A unit variant inside a constructor folds the nested match too.
    let nested = "enum Color { Red, Green } \
                  fn f() -> int { let o = Option::Some(Color::Green); \
                  return match o { Option::Some(c) => match c { Color::Red => 1, Color::Green => 2, }, Option::None => 0, }; }";
    assert_eq!(m(nested), 0);
    // Written again, or bound to something else, it stays.
    assert_eq!(m("fn f(int i) -> int { let o = Option::Some(i); o = Option::None; return match o { Option::Some(x) => x, Option::None => 0, }; }"), 1);
    assert_eq!(m("fn f(int i) -> int { let o = Option::Some(i); i = 2; return match o { Option::Some(x) => x, Option::None => 0, }; }"), 1);
}
