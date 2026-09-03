use super::*;
use crate::il::op::{IlJumpKind, IlOp, Label};
use crate::il::opt::{optimize, OptimizeOptions};
use common::{DebugLoc, Instruction};

fn loc() -> DebugLoc {
    DebugLoc::unknown()
}

/// `i = 0; while i < 10 { t = 42; i = i + 1 } ; return t`
fn loop_with_invariant_store(read_after: bool, read_in_loop: bool, variant: bool) -> Vec<IlOp> {
    let mut ops = vec![
        IlOp::Const { imm: 0, loc: loc() },
        IlOp::StorePop {
            slot: 0,
            loc: loc(),
        },
        IlOp::Label(Label(0)),
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const {
            imm: 10,
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
    ];
    if variant {
        ops.extend([
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
        ]);
    } else {
        ops.extend([
            IlOp::Const {
                imm: 42,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
        ]);
    }
    if read_in_loop {
        ops.extend([
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
        ]);
    }
    ops.extend([
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const { imm: 1, loc: loc() },
        IlOp::Bin {
            op: Instruction::ADD,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 0,
            loc: loc(),
        },
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: loc(),
            hint: Default::default(),
        },
        IlOp::Label(Label(1)),
    ]);
    if read_after {
        ops.push(IlOp::Load {
            slot: 1,
            loc: loc(),
        });
        ops.push(IlOp::Return { loc: loc(), ret_words: 1});
    } else {
        ops.push(IlOp::Halt { loc: loc() });
    }
    ops
}

fn stores_to_slot_in_loop(ops: &[IlOp], slot: u32) -> usize {
    let header = ops
        .iter()
        .position(|op| matches!(op, IlOp::Label(Label(0))))
        .unwrap();
    let latch = ops
        .iter()
        .rposition(|op| {
            matches!(
                op,
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(0),
                    ..
                }
            )
        })
        .unwrap();
    ops[header..=latch]
        .iter()
        .filter(|op| matches!(op, IlOp::StorePop { slot: s, .. } if *s == slot))
        .count()
}

#[test]
fn eliminates_unused_invariant_store() {
    let mut ops = loop_with_invariant_store(false, false, false);
    eliminate_invariant_stores(&mut ops, 0);
    assert_eq!(stores_to_slot_in_loop(&ops, 1), 0);
    assert!(!ops
        .iter()
        .any(|op| matches!(op, IlOp::StorePop { slot: 1, .. })));
}

#[test]
fn sinks_live_invariant_store_out_of_loop() {
    let mut ops = loop_with_invariant_store(true, false, false);
    eliminate_invariant_stores(&mut ops, 0);
    assert_eq!(stores_to_slot_in_loop(&ops, 1), 0);
    assert!(
        ops.iter()
            .any(|op| matches!(op, IlOp::StorePop { slot: 1, .. })),
        "live-after store must be sunk, not dropped"
    );
}

#[test]
fn keeps_variant_store() {
    let mut ops = loop_with_invariant_store(true, false, true);
    eliminate_invariant_stores(&mut ops, 0);
    assert_eq!(stores_to_slot_in_loop(&ops, 1), 1);
}

#[test]
fn keeps_store_read_in_loop() {
    let mut ops = loop_with_invariant_store(false, true, false);
    eliminate_invariant_stores(&mut ops, 0);
    assert_eq!(stores_to_slot_in_loop(&ops, 1), 1);
}

#[test]
fn eliminates_two_unused_invariant_stores() {
    let mut ops = vec![
        IlOp::Const { imm: 0, loc: loc() },
        IlOp::StorePop {
            slot: 0,
            loc: loc(),
        },
        IlOp::Label(Label(0)),
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const {
            imm: 10,
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
        IlOp::Const { imm: 1, loc: loc() },
        IlOp::StorePop {
            slot: 2,
            loc: loc(),
        },
        IlOp::Const { imm: 2, loc: loc() },
        IlOp::StorePop {
            slot: 3,
            loc: loc(),
        },
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const { imm: 1, loc: loc() },
        IlOp::Bin {
            op: Instruction::ADD,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 0,
            loc: loc(),
        },
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: loc(),
            hint: Default::default(),
        },
        IlOp::Label(Label(1)),
        IlOp::Halt { loc: loc() },
    ];
    eliminate_invariant_stores(&mut ops, 0);
    assert_eq!(stores_to_slot_in_loop(&ops, 2), 0);
    assert_eq!(stores_to_slot_in_loop(&ops, 3), 0);
}

#[test]
fn optimize_pipeline_eliminates_unused_invariant_store() {
    let mut ops = loop_with_invariant_store(false, false, false);
    optimize(&mut ops, &OptimizeOptions::default(), &mut Vec::new());
    assert_eq!(stores_to_slot_in_loop(&ops, 1), 0);
}
