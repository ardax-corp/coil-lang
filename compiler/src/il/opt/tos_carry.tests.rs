use super::*;
use crate::il::op::IlOp;
use common::{DebugLoc, Instruction};

fn loc() -> DebugLoc {
    DebugLoc::unknown()
}

fn addf() -> u8 {
    Instruction::ADDF as u8
}

fn mulf() -> u8 {
    Instruction::MULF as u8
}

/// `tr = …; zi = zr + zi; zr = tr`
fn latch_copy() -> Vec<IlOp> {
    vec![
        IlOp::Const { imm: 1, loc: loc() },
        IlOp::StorePop {
            slot: 5,
            loc: loc(),
        },
        IlOp::BinSlotSlot {
            op: addf(),
            a: 7,
            b: 6,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 6,
            loc: loc(),
        },
        IlOp::Load {
            slot: 5,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 7,
            loc: loc(),
        },
        IlOp::Return { loc: loc(), ret_words: 1},
    ]
}

#[test]
fn delays_store_across_bin_slot_then_drops_reload() {
    let mut ops = latch_copy();
    assert_eq!(tos_carry(&mut ops, 8), 1);
    assert!(!ops.iter().any(|op| matches!(op, IlOp::Load { slot: 5, .. })));
    assert_eq!(
        ops.iter()
            .filter(|op| matches!(op, IlOp::StorePop { slot: 5, .. }))
            .count(),
        0
    );
    assert!(matches!(
        &ops[..],
        [
            IlOp::Const { .. },
            IlOp::BinSlotSlot { .. },
            IlOp::StorePop { slot: 6, .. },
            IlOp::StorePop { slot: 7, .. },
            IlOp::Return { .. },
        ]
    ));
}

#[test]
fn refuses_load_of_carried_slot_in_region() {
    let mut ops = vec![
        IlOp::Const { imm: 1, loc: loc() },
        IlOp::StorePop {
            slot: 5,
            loc: loc(),
        },
        IlOp::Load {
            slot: 5,
            loc: loc(),
        },
        IlOp::Pop { loc: loc() },
        IlOp::Load {
            slot: 5,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 7,
            loc: loc(),
        },
        IlOp::Return { loc: loc(), ret_words: 1},
    ];
    assert_eq!(tos_carry(&mut ops, 8), 0);
}

#[test]
fn refuses_store_back_to_carried_slot() {
    let mut ops = vec![
        IlOp::Const { imm: 1, loc: loc() },
        IlOp::StorePop {
            slot: 5,
            loc: loc(),
        },
        IlOp::BinSlotSlot {
            op: addf(),
            a: 7,
            b: 6,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 5,
            loc: loc(),
        },
        IlOp::Load {
            slot: 5,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 7,
            loc: loc(),
        },
        IlOp::Return { loc: loc(), ret_words: 1},
    ];
    assert_eq!(tos_carry(&mut ops, 8), 0);
}

#[test]
fn refuses_control_flow_in_region() {
    let mut ops = vec![
        IlOp::Const { imm: 1, loc: loc() },
        IlOp::StorePop {
            slot: 5,
            loc: loc(),
        },
        IlOp::Label(crate::il::op::Label(0)),
        IlOp::BinSlotSlot {
            op: addf(),
            a: 7,
            b: 6,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 6,
            loc: loc(),
        },
        IlOp::Load {
            slot: 5,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 7,
            loc: loc(),
        },
        IlOp::Return { loc: loc(), ret_words: 1},
    ];
    assert_eq!(tos_carry(&mut ops, 8), 0);
}

#[test]
fn refuses_bin_slot_that_reads_carried_slot() {
    let mut ops = vec![
        IlOp::Const { imm: 1, loc: loc() },
        IlOp::StorePop {
            slot: 5,
            loc: loc(),
        },
        IlOp::BinSlotSlot {
            op: addf(),
            a: 5,
            b: 6,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 6,
            loc: loc(),
        },
        IlOp::Load {
            slot: 5,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 7,
            loc: loc(),
        },
        IlOp::Return { loc: loc(), ret_words: 1},
    ];
    assert_eq!(tos_carry(&mut ops, 8), 0);
}

/// `tr = …; zi = 2 * zr * zi + ci; zr = tr` (mandelbrot latch).
fn mandelbrot_latch() -> Vec<IlOp> {
    vec![
        IlOp::Const { imm: 1, loc: loc() },
        IlOp::StorePop {
            slot: 12,
            loc: loc(),
        },
        IlOp::Const { imm: 2, loc: loc() },
        IlOp::BinSlotSlot {
            op: mulf(),
            a: 7,
            b: 8,
            loc: loc(),
        },
        IlOp::Bin {
            op: Instruction::MULF,
            loc: loc(),
        },
        IlOp::Load {
            slot: 15,
            loc: loc(),
        },
        IlOp::Bin {
            op: Instruction::ADDF,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 8,
            loc: loc(),
        },
        IlOp::Load {
            slot: 12,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 7,
            loc: loc(),
        },
        IlOp::Return { loc: loc(), ret_words: 1},
    ]
}

#[test]
fn delays_store_across_const_and_stack_bin() {
    let mut ops = mandelbrot_latch();
    assert_eq!(tos_carry(&mut ops, 16), 1);
    assert!(!ops.iter().any(|op| matches!(op, IlOp::Load { slot: 12, .. })));
    assert_eq!(
        ops.iter()
            .filter(|op| matches!(op, IlOp::StorePop { slot: 12, .. }))
            .count(),
        0
    );
    assert!(matches!(
        &ops[..],
        [
            IlOp::Const { .. },
            IlOp::Const { .. },
            IlOp::BinSlotSlot { .. },
            IlOp::Bin { .. },
            IlOp::Load { slot: 15, .. },
            IlOp::Bin { .. },
            IlOp::StorePop { slot: 8, .. },
            IlOp::StorePop { slot: 7, .. },
            IlOp::Return { .. },
        ]
    ));
}
