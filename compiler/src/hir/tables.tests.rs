//! Which table reads [`fold_tables`] folds.

use super::*;
use crate::hir::build_module;
use crate::typechecking::infer::Checker;

/// No module tables.
struct NoGlobals;

impl GlobalTables for NoGlobals {
    fn len(&self, _: HirId) -> Option<usize> {
        None
    }

    fn elem(&self, _: HirId, _: usize, _: HirId) -> Option<Lit> {
        None
    }
}

/// `f`'s body as built, and after [`fold_tables`] with every local sealed.
fn folded(src: &str) -> (HirBody, HirBody) {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let body = module.bodies.into_iter().find(|b| b.name == "f").expect("body");
    let out = fold_tables(&body, &NoGlobals, &|_| true).unwrap_or_else(|| body.clone());
    (body, out)
}

/// How many reachable nodes of `body` `pick` accepts.
fn count(body: &HirBody, pick: impl Fn(&HirKind) -> bool) -> usize {
    let mut n = 0;
    let mut stack = vec![body.root.expect("root")];
    while let Some(id) = stack.pop() {
        n += usize::from(pick(&body.expr(id).kind));
        stack.extend(children(body, id));
    }
    n
}

fn index(k: &HirKind) -> bool {
    matches!(k, HirKind::Index { .. })
}

fn make_array(k: &HirKind) -> bool {
    matches!(k, HirKind::Make { kind: MakeKind::Array, .. })
}

#[test]
fn constant_index_reads_fold_and_the_table_goes() {
    let (_, out) = folded("fn f() -> int { const t = readonly [4, 5, 6]; return t[0] + t[2] + t.len(); }");
    assert_eq!(count(&out, index), 0);
    assert_eq!(count(&out, make_array), 0, "nothing reads the table any more");
}

#[test]
fn a_variable_index_keeps_the_table() {
    let (_, out) = folded("fn f(int i) -> int { const t = readonly [4, 5, 6]; return t[i] + t[1]; }");
    assert_eq!(count(&out, index), 1);
    assert_eq!(count(&out, make_array), 1);
}

#[test]
fn an_index_out_of_range_stays() {
    let (_, out) = folded("fn f() -> int { const t = readonly [4, 5, 6]; return t[3]; }");
    assert_eq!(count(&out, index), 1, "the read still traps at run time");
}

#[test]
fn a_table_with_a_computed_element_is_not_one() {
    let (body, out) = folded("fn f(int x) -> int { const t = readonly [4, x, 6]; return t[0]; }");
    assert_eq!(count(&out, index), count(&body, index));
}

#[test]
fn a_let_is_not_a_table() {
    let (body, out) = folded("fn f() -> int { let t = readonly [4, 5, 6]; return t[0]; }");
    assert_eq!(count(&out, index), count(&body, index));
}

#[test]
fn lets_inside_loops_are_found() {
    let (body, _) = folded("fn f() -> int { const a = readonly [1]; let s = 0; for i in 0..3 { const b = readonly [2]; s += b[0]; } return s + a[0]; }");
    let looped = lets_in_loops(&body);
    let names: Vec<String> = looped
        .iter()
        .filter_map(|&id| match body.expr(id).kind {
            HirKind::Let { local, .. } => Some(body.local(local).name.clone()),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"b".to_string()), "{names:?}");
    assert!(!names.contains(&"a".to_string()), "{names:?}");
}
