use super::*;
use crate::il::op::{IlJumpKind, IlOp, Label};
use common::{DebugLoc, Instruction};

fn loc() -> DebugLoc {
    DebugLoc::unknown()
}

/// `t = x + y` before a diamond; recompute at the join → `Load t`.
fn diamond_recompute_at_join() -> Vec<IlOp> {
    vec![
        IlOp::Const { imm: 3, loc: loc() },
        IlOp::StorePop {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const { imm: 4, loc: loc() },
        IlOp::StorePop {
            slot: 1,
            loc: loc(),
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
            slot: 2,
            loc: loc(),
        },
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(1),
            loc: loc(),
            hint: Default::default(),
        },
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(2),
            loc: loc(),
            hint: Default::default(),
        },
        IlOp::Label(Label(1)),
        IlOp::Const { imm: 9, loc: loc() },
        IlOp::Pop { loc: loc() },
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(2),
            loc: loc(),
            hint: Default::default(),
        },
        IlOp::Label(Label(2)),
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
            slot: 3,
            loc: loc(),
        },
        IlOp::Halt { loc: loc() },
    ]
}

#[test]
fn ssa_gvn_cse_across_basic_blocks() {
    let mut ops = diamond_recompute_at_join();
    ssa_gvn(&mut ops);
    let join = ops
        .iter()
        .position(|op| matches!(op, IlOp::Label(Label(2))))
        .unwrap();
    assert!(
        !ops[join..].iter().any(|op| matches!(op, IlOp::Bin { .. })),
        "join should Load the already-stored sum, not recompute ADD"
    );
    assert!(ops[join..]
        .iter()
        .any(|op| matches!(op, IlOp::Load { slot: 2, .. })));
}

#[test]
fn ssa_gvn_cse_at_join_when_preds_agree() {
    let mut ops = vec![
        IlOp::Const { imm: 3, loc: loc() },
        IlOp::StorePop {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const { imm: 4, loc: loc() },
        IlOp::StorePop {
            slot: 1,
            loc: loc(),
        },
        IlOp::Load {
            slot: 0,
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
            slot: 2,
            loc: loc(),
        },
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(2),
            loc: loc(),
            hint: Default::default(),
        },
        IlOp::Label(Label(1)),
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
            slot: 2,
            loc: loc(),
        },
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(2),
            loc: loc(),
            hint: Default::default(),
        },
        IlOp::Label(Label(2)),
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
        IlOp::Return { loc: loc() },
    ];
    ssa_gvn(&mut ops);
    let join = ops
        .iter()
        .position(|op| matches!(op, IlOp::Label(Label(2))))
        .unwrap();
    assert!(!ops[join..].iter().any(|op| matches!(op, IlOp::Bin { .. })));
    assert!(ops[join..]
        .iter()
        .any(|op| matches!(op, IlOp::Load { slot: 2, .. })));
}

#[test]
fn ssa_gvn_preserves_div() {
    let mut ops = vec![
        IlOp::Const { imm: 8, loc: loc() },
        IlOp::StorePop {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const { imm: 2, loc: loc() },
        IlOp::StorePop {
            slot: 1,
            loc: loc(),
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
            op: Instruction::DIV,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 2,
            loc: loc(),
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
            op: Instruction::DIV,
            loc: loc(),
        },
        IlOp::Return { loc: loc() },
    ];
    ssa_gvn(&mut ops);
    let divs = ops
        .iter()
        .filter(|op| {
            matches!(
                op,
                IlOp::Bin {
                    op: Instruction::DIV,
                    ..
                }
            )
        })
        .count();
    assert_eq!(divs, 2);
}

#[test]
fn ssa_gvn_skips_join_when_operand_phi_disagrees() {
    let mut ops = vec![
        IlOp::Const { imm: 3, loc: loc() },
        IlOp::StorePop {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const { imm: 4, loc: loc() },
        IlOp::StorePop {
            slot: 1,
            loc: loc(),
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
            slot: 2,
            loc: loc(),
        },
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(1),
            loc: loc(),
            hint: Default::default(),
        },
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(2),
            loc: loc(),
            hint: Default::default(),
        },
        IlOp::Label(Label(1)),
        IlOp::Const {
            imm: 99,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 0,
            loc: loc(),
        },
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(2),
            loc: loc(),
            hint: Default::default(),
        },
        IlOp::Label(Label(2)),
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
            slot: 3,
            loc: loc(),
        },
        IlOp::Halt { loc: loc() },
    ];
    ssa_gvn(&mut ops);
    let join = ops
        .iter()
        .position(|op| matches!(op, IlOp::Label(Label(2))))
        .unwrap();
    assert!(
        ops[join..].iter().any(|op| matches!(op, IlOp::Bin { .. })),
        "phi on slot 0 must keep the join ADD"
    );
}

#[test]
fn build_ssa_records_phi_on_disagreeing_join() {
    let ops = vec![
        IlOp::Const { imm: 1, loc: loc() },
        IlOp::StorePop {
            slot: 0,
            loc: loc(),
        },
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(1),
            loc: loc(),
            hint: Default::default(),
        },
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(2),
            loc: loc(),
            hint: Default::default(),
        },
        IlOp::Label(Label(1)),
        IlOp::Const { imm: 2, loc: loc() },
        IlOp::StorePop {
            slot: 0,
            loc: loc(),
        },
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(2),
            loc: loc(),
            hint: Default::default(),
        },
        IlOp::Label(Label(2)),
        IlOp::Halt { loc: loc() },
    ];
    let ssa = build_ssa(&ops);
    let join = ssa
        .blocks
        .iter()
        .position(|&(s, _)| matches!(ops.get(s), Some(IlOp::Label(Label(2)))))
        .unwrap();
    assert!(ssa.preds[join].len() >= 2);
    assert!(ssa.slot_in[join].contains_key(&0));
}

#[test]
fn number_and_eliminate_api() {
    let mut ops = diamond_recompute_at_join();
    let ssa = build_ssa(&ops);
    let vns = number_values(&ssa);
    assert_eq!(vns.produced.len(), ops.len());
    eliminate_redundant(&mut ops, &vns);
}
