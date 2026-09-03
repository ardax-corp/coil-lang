use super::*;
use crate::il::op::{IlJumpKind, IlOp, Label};
use crate::il::opt::{OptimizeOptions, optimize};
use common::{DebugLoc, Instruction};

fn loc() -> DebugLoc {
    DebugLoc::unknown()
}

/// `i = 0; while i < 3 { s = s + i; i = i + 1 }`
fn counted_while(trips_imm: i32, with_call: bool, with_break: bool) -> Vec<IlOp> {
    let mut ops = vec![
        IlOp::Const { imm: 0, loc: loc() },
        IlOp::StorePop {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const { imm: 0, loc: loc() },
        IlOp::StorePop {
            slot: 1,
            loc: loc(),
        },
        IlOp::Label(Label(0)),
        IlOp::Load {
            slot: 1,
            loc: loc(),
        },
        IlOp::Const {
            imm: trips_imm,
            loc: loc(),
        },
        IlOp::Bin {
            op: Instruction::LE,
            loc: loc(),
        },
        IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(1),
            loc: loc(),
            hint: Default::default(),
        },
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Load {
            slot: 1,
            loc: loc(),
        },
        IlOp::Bin {
            op: Instruction::ADD,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 0,
            loc: loc(),
        },
    ];
    if with_call {
        ops.push(IlOp::Entry {
            kind: crate::il::op::EntryKind::Call,
            arity: 0,
            target: Label(9),
            loc: loc(),
        ret_words: 1,
        });
        ops.push(IlOp::Pop { loc: loc() });
    }
    if with_break {
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(1),
            loc: loc(),
            hint: Default::default(),
        });
    }
    ops.extend([
        IlOp::Load {
            slot: 1,
            loc: loc(),
        },
        IlOp::Const { imm: 1, loc: loc() },
        IlOp::Bin {
            op: Instruction::ADD,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 1,
            loc: loc(),
        },
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: loc(),
            hint: Default::default(),
        },
        IlOp::Label(Label(1)),
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Return { loc: loc() },
    ]);
    ops
}

fn has_back_edge_to_header(ops: &[IlOp]) -> bool {
    ops.iter().any(|op| {
        matches!(
            op,
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                ..
            }
        )
    })
}

fn add_count(ops: &[IlOp]) -> usize {
    ops.iter()
        .filter(|op| {
            matches!(
                op,
                IlOp::Bin {
                    op: Instruction::ADD,
                    ..
                }
            )
        })
        .count()
}

#[test]
fn unrolls_simple_const_bound_while() {
    let mut ops = counted_while(3, false, false);
    unroll_loops(&mut ops, 8);
    assert!(
        !has_back_edge_to_header(&ops),
        "full unroll must drop the latch JMP"
    );
    assert!(
        !ops.iter().any(|op| matches!(
            op,
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                ..
            }
        )),
        "full unroll must drop the header JMPF"
    );
    // Body ADD + induction ADD, three trips.
    assert_eq!(add_count(&ops), 6);
}

#[test]
fn range_shaped_loop_unrolls_like_counted_for_in() {
    // Same IL as `for x in 0..3` after a non-unrolling codegen: i from 0, i < 3.
    let mut ops = counted_while(3, false, false);
    let found = find_unrollable_loops(&ops);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].trips, 3);
    unroll_loop(&mut ops, &found[0]);
    assert!(!has_back_edge_to_header(&ops));
}

#[test]
fn break_disables_unroll() {
    let mut ops = counted_while(3, false, true);
    unroll_loops(&mut ops, 8);
    assert!(
        has_back_edge_to_header(&ops),
        "break is an extra exit; loop must stay"
    );
}

#[test]
fn call_disables_unroll() {
    let mut ops = counted_while(3, true, false);
    unroll_loops(&mut ops, 8);
    assert!(has_back_edge_to_header(&ops));
}

#[test]
fn nested_loops_are_not_unrolled() {
    let loc = loc();
    let mut ops = vec![
        IlOp::Const { imm: 0, loc },
        IlOp::StorePop { slot: 0, loc },
        IlOp::Const { imm: 0, loc },
        IlOp::StorePop { slot: 1, loc },
        IlOp::Label(Label(0)),
        IlOp::Load { slot: 0, loc },
        IlOp::Const { imm: 2, loc },
        IlOp::Bin {
            op: Instruction::LE,
            loc,
        },
        IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(3),
            loc,
            hint: Default::default(),
        },
        IlOp::Const { imm: 0, loc },
        IlOp::StorePop { slot: 1, loc },
        IlOp::Label(Label(1)),
        IlOp::Load { slot: 1, loc },
        IlOp::Const { imm: 2, loc },
        IlOp::Bin {
            op: Instruction::LE,
            loc,
        },
        IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(2),
            loc,
            hint: Default::default(),
        },
        IlOp::Load { slot: 1, loc },
        IlOp::Const { imm: 1, loc },
        IlOp::Bin {
            op: Instruction::ADD,
            loc,
        },
        IlOp::StorePop { slot: 1, loc },
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(1),
            loc,
            hint: Default::default(),
        },
        IlOp::Label(Label(2)),
        IlOp::Load { slot: 0, loc },
        IlOp::Const { imm: 1, loc },
        IlOp::Bin {
            op: Instruction::ADD,
            loc,
        },
        IlOp::StorePop { slot: 0, loc },
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc,
            hint: Default::default(),
        },
        IlOp::Label(Label(3)),
        IlOp::Halt { loc },
    ];
    unroll_loops(&mut ops, 8);
    assert!(
        find_unrollable_loops(&ops).is_empty() || has_back_edge_to_header(&ops),
        "nested counted loops must stay rolled"
    );
    let latches = ops
        .iter()
        .filter(|op| {
            matches!(
                op,
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    ..
                }
            )
        })
        .count();
    assert_eq!(latches, 2, "both latches must survive");
}

#[test]
fn trip_count_above_eight_is_not_unrolled() {
    let mut ops = counted_while(9, false, false);
    unroll_loops(&mut ops, 8);
    assert!(has_back_edge_to_header(&ops));
}

#[test]
fn factor_smaller_than_trips_skips() {
    let mut ops = counted_while(5, false, false);
    unroll_loops(&mut ops, 4);
    assert!(has_back_edge_to_header(&ops));
    unroll_loops(&mut ops, 5);
    assert!(!has_back_edge_to_header(&ops));
}

#[test]
fn pgo_prefer_hot_without_profile_still_unrolls() {
    let mut ops = counted_while(3, false, false);
    unroll_loops_pgo(&mut ops, 8, true);
    assert!(!has_back_edge_to_header(&ops));
}

#[test]
fn optimize_pipeline_unrolls_counted_while() {
    let mut ops = counted_while(3, false, false);
    optimize(&mut ops, &OptimizeOptions::default(), &mut Vec::new());
    assert!(
        !has_back_edge_to_header(&ops),
        "default optimize must run loop_unroll"
    );
}
