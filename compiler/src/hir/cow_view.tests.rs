//! Which loops get a view: the copy-on-write ops left inside a loop that
//! writes through a nested path. Values are checked by
//! `tests/memory/cow_loop_path_writes.coil`.

use crate::hir::effects::{ModuleEffects, ProgramEffects};
use crate::hir::{build_module, Builtin, HirBody, HirId, HirKind};
use crate::typechecking::infer::Checker;

const CLASSES: &str = "
class Block { pub insts: Vec<int>, pub tag: int, }
class Func { pub blocks: Vec<Block>, }
static let HITS: int = 0;
fn hit() { HITS += 1; }
";

/// `f` after copy-on-write: the number of `unique` ops inside each loop
/// of it, outermost first.
fn uniques_in_loops(src: &str) -> Vec<usize> {
    let src = format!("{CLASSES}{src}");
    let owned = Box::leak(src.into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let mut program = ProgramEffects::default();
    let summaries = ModuleEffects::solve(&module, &checker, "", &program).summaries;
    program.record(&module, &checker, "", &summaries);
    let i = module.bodies.iter().position(|b| b.name == "f").expect("body `f`");
    let (out, notes, _) = program.cow_rewrite(&checker, "", &module, &module.bodies[i], summaries[i].mutates);
    assert!(notes.is_empty(), "{notes:?}");
    let body = out.unwrap_or_else(|| module.bodies[i].clone());
    let mut loops = Vec::new();
    body.visit_ids(body.root.expect("root"), &mut |id| {
        if matches!(body.expr(id).kind, HirKind::Loop { .. } | HirKind::ForIn { .. }) {
            loops.push(uniques(&body, id));
        }
    });
    loops
}

fn uniques(body: &HirBody, at: HirId) -> usize {
    let mut n = 0;
    body.visit(at, &mut |e| n += usize::from(matches!(e.kind, HirKind::Builtin { op: Builtin::Cow { unique: true }, .. })));
    n
}

#[test]
fn a_nested_path_write_makes_nothing_unique_in_its_loop() {
    let src = "fn f(Func f) {
        let b = 0;
        while b < f.blocks.len() {
            let i = 0;
            while i < f.blocks[b].insts.len() {
                f.blocks[b].insts[i] = f.blocks[b].insts[i] * 2;
                i = i + 1;
            }
            b = b + 1;
        }
    }";
    // The outer loop makes `blocks`, `blocks[b]` and its `insts` unique
    // once per block, for the inner loop's view. Its own write through
    // `blocks` is in the inner loop, which may run no iteration: no view.
    assert_eq!(uniques_in_loops(src), vec![3, 0]);
    let src = "fn f(Func f) {
        let b = 0;
        while b < f.blocks.len() {
            let i = 0;
            while i < f.blocks[b].insts.len() {
                f.blocks[b].insts[i] = f.blocks[b].insts[i] * 2;
                i = i + 1;
            }
            f.blocks[b].insts.push(7);
            b = b + 1;
        }
    }";
    // With a write in every outer iteration, `blocks` is made unique once
    // per call: two in the outer loop for the inner view, two for `push`.
    assert_eq!(uniques_in_loops(src), vec![4, 0]);
    let src = "fn f(Func f, int b) {
        for i in 0..f.blocks[b].insts.len() {
            f.blocks[b].insts[i] += 1;
        }
    }";
    assert_eq!(uniques_in_loops(src), vec![0]);
}

#[test]
fn a_loop_that_replaces_or_shares_part_of_the_path_keeps_its_uniques() {
    let replaced = "fn f(Func f) {
        let i = 0;
        while i < f.blocks[0].insts.len() {
            f.blocks[0].insts[i] = 1;
            f.blocks[0] = new Block(Vec::new(), 0);
            i = i + 1;
        }
    }";
    let shared = "fn f(Func f, Vec<Block> out) {
        let i = 0;
        while i < f.blocks[0].insts.len() {
            f.blocks[0].insts[i] = 1;
            out.push(f.blocks[0]);
            i = i + 1;
        }
    }";
    let index_moves = "fn f(Func f, int b) {
        let i = 0;
        while i < f.blocks[b].insts.len() {
            f.blocks[b].insts[i] = 1;
            b = i % 2;
            i = i + 1;
        }
    }";
    for src in [replaced, shared, index_moves] {
        assert!(uniques_in_loops(src)[0] >= 2, "{src}");
    }
}

#[test]
fn a_parameter_root_keeps_its_uniques_across_a_call_that_touches_a_static() {
    // The caller's place may be a `static` the call writes.
    let src = "fn f(Func f) {
        let i = 0;
        while i < f.blocks[0].insts.len() {
            f.blocks[0].insts[i] = 1;
            hit();
            i = i + 1;
        }
    }";
    assert!(uniques_in_loops(src)[0] >= 2);
    // A local the body made is no static's.
    let src = "fn f() {
        let f = new Func(Vec::new());
        f.blocks.push(new Block(Vec::new(), 0));
        let i = 0;
        while i < f.blocks[0].insts.len() {
            f.blocks[0].insts[i] = 1;
            hit();
            i = i + 1;
        }
    }";
    assert_eq!(uniques_in_loops(src), vec![0]);
}

#[test]
fn a_path_whose_index_the_loop_may_not_reach_is_not_read_ahead() {
    // `blocks[b]` may be out of bounds: the loop's test does not read it.
    let src = "fn f(Func f, int b, int n) {
        let i = 0;
        while i < n {
            f.blocks[b].insts[0] = i;
            i = i + 1;
        }
    }";
    assert!(uniques_in_loops(src)[0] >= 2);
}

#[test]
fn a_write_that_may_not_run_in_an_iteration_keeps_its_uniques() {
    // A view would make the path unique where the loop never writes it.
    let branch = "fn f(Func f) {
        for i in 0..f.blocks[0].insts.len() {
            if f.blocks[0].insts[i] < 0 {
                f.blocks[0].insts[i] = 0;
            }
        }
    }";
    let right_of_and = "fn f(Func f) {
        for i in 0..f.blocks[0].insts.len() {
            let _ = i > 3 && f.blocks[0].insts.pop() > 0;
        }
    }";
    let after_continue = "fn f(Func f) {
        let i = 0;
        while i < f.blocks[0].insts.len() {
            i = i + 1;
            if f.blocks[0].insts[i - 1] >= 0 {
                continue;
            }
            f.blocks[0].insts[i - 1] = 0;
        }
    }";
    let after_try = "fn get(int i) -> Option<int> { if i > 2 { return Option::None; } return Option::Some(i); }
    fn f(Func f) -> Option<int> {
        for i in 0..f.blocks[0].insts.len() {
            let v = get(i)?;
            f.blocks[0].insts[i] = v;
        }
        return Option::Some(0);
    }";
    for src in [branch, right_of_and, after_continue, after_try] {
        assert!(uniques_in_loops(src)[0] >= 2, "{src}");
    }
}
