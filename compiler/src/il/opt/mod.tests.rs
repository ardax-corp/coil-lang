//! Iterative optimization / convergence (COI-130).

use super::*;
use crate::il::op::{IlJumpKind, IlOp, Label};
use common::DebugLoc;

fn loc() -> DebugLoc {
    DebugLoc::unknown()
}

fn jmp(id: u32) -> IlOp {
    IlOp::Jump {
        kind: IlJumpKind::Unconditional,
        target: Label(id),
        loc: loc(),
    }
}

fn label(id: u32) -> IlOp {
    IlOp::Label(Label(id))
}

fn ret() -> IlOp {
    IlOp::Return { loc: loc() }
}

fn c(n: i32) -> IlOp {
    IlOp::Const {
        imm: n,
        loc: loc(),
    }
}

fn entry_target(ops: &[IlOp]) -> Option<u32> {
    match ops.first() {
        Some(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(id),
            ..
        }) => Some(*id),
        _ => None,
    }
}

/// Jump threading only. One hop per jump per round, so a 3-edge chain
/// still has work after a single pipeline pass.
fn jump_thread_opts() -> OptimizeOptions {
    let mut o = super::OptLevel::None.options();
    o.algebraic = false;
    o.jump_thread = true;
    o.iterative_optimization = false;
    o
}

/// JMP L1; L1: JMP L2; L2: JMP L3; L3: RET
fn jmp_chain() -> Vec<IlOp> {
    vec![
        jmp(1),
        label(1),
        jmp(2),
        label(2),
        jmp(3),
        label(3),
        ret(),
    ]
}

#[test]
fn simple_code_converges_in_one_round() {
    let mut ops = vec![c(1), ret()];
    let stats = optimize_iteratively(&mut ops, &OptimizeOptions::default(), &mut Vec::new(), 10);
    assert_eq!(stats.iterations, 1);
    assert!(stats.converged);
    assert!(!stats.hit_iteration_limit);
    assert_eq!(stats.passes.len(), 1);
    assert!(!stats.passes[0].changed);
}

#[test]
fn jmp_chain_needs_two_rounds_to_thread_to_the_return() {
    let mut ops = jmp_chain();
    let stats = optimize_iteratively(&mut ops, &jump_thread_opts(), &mut Vec::new(), 10);
    assert!(stats.converged);
    assert!(!stats.hit_iteration_limit);
    assert!(
        stats.iterations >= 2,
        "jump_thread follows one hop per round; a 3-edge chain needs a second pass"
    );
    assert!(stats.passes.iter().filter(|p| p.changed).count() >= 2);
    assert_eq!(entry_target(&ops), Some(3));
}

#[test]
fn max_iterations_stops_before_a_fixed_point() {
    let mut ops = jmp_chain();
    let stats = optimize_iteratively(&mut ops, &jump_thread_opts(), &mut Vec::new(), 1);
    assert_eq!(stats.iterations, 1);
    assert_eq!(stats.passes.len(), 1);
    assert!(stats.passes[0].changed);
    assert!(!stats.converged);
    assert!(stats.hit_iteration_limit);
    assert_eq!(entry_target(&ops), Some(2));
}

#[test]
fn optimize_respects_iterative_flag() {
    let mut once = jmp_chain();
    let mut looped = jmp_chain();
    let mut opts = jump_thread_opts();
    opts.iterative_optimization = false;
    optimize(&mut once, &opts, &mut Vec::new());
    opts.iterative_optimization = true;
    opts.max_optimization_iterations = 10;
    optimize(&mut looped, &opts, &mut Vec::new());
    assert_eq!(entry_target(&once), Some(2));
    assert_eq!(entry_target(&looped), Some(3));
}

#[test]
fn run_optimization_pass_matches_a_single_optimize() {
    let mut a = jmp_chain();
    let mut b = jmp_chain();
    let opts = jump_thread_opts();
    let stats = run_optimization_pass(&mut a, &opts, &mut Vec::new());
    optimize(&mut b, &opts, &mut Vec::new());
    assert!(stats.changed);
    assert!(a == b, "single pass and optimize() should match");
    assert_eq!(entry_target(&a), Some(2));
}
