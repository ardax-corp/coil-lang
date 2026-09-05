use super::copy_prop_for_test as copy_prop;
use super::dead_store_at_for_test as dead_store_at;
use super::dest_prop;
use crate::il::op::{IlJumpKind, IlOp, Label};
use common::{Byte, DebugLoc, Instruction};

fn loc() -> DebugLoc {
    DebugLoc::unknown()
}

fn load(slot: u32) -> IlOp {
    IlOp::Load { slot, loc: loc() }
}

fn store(slot: u32) -> IlOp {
    IlOp::StorePop {
        slot,
        loc: loc(),
    }
}

#[test]
fn dest_prop_forwards_load_across_getfield() {
    let mut ops = vec![
        load(0),
        store(2),
        load(2),
        IlOp::GetField { loc: loc() },
        load(2),
        IlOp::Return {
            loc: loc(),
            ret_words: 1,
        },
    ];
    dest_prop(&mut ops, 3);
    assert!(
        matches!(ops[2], IlOp::Load { slot: 0, .. }),
        "GetField object should load src"
    );
    assert!(
        matches!(ops[4], IlOp::Load { slot: 0, .. }),
        "post-GetField use should load src"
    );
}

#[test]
fn dest_prop_forwards_load_across_make_enum() {
    let mut ops = vec![
        load(0),
        store(3),
        load(3),
        IlOp::MakeEnum {
            tag: 1,
            arity: 1,
            loc: loc(),
        },
        load(3),
        store(4),
        IlOp::Return {
            loc: loc(),
            ret_words: 1,
        },
    ];
    dest_prop(&mut ops, 3);
    assert!(matches!(ops[2], IlOp::Load { slot: 0, .. }));
    assert!(matches!(ops[4], IlOp::Load { slot: 0, .. }));
    assert!(matches!(ops[5], IlOp::StorePop { slot: 4, .. }));
}

#[test]
fn dest_prop_forwards_bin_slot_across_set_field() {
    let mut ops = vec![
        load(1),
        store(5),
        load(5),
        load(2),
        IlOp::SetField {
            index: Some(0),
            loc: loc(),
        },
        IlOp::BinSlotImm {
            op: Instruction::ADD as u8,
            slot: 5,
            imm: 1,
            loc: loc(),
        },
        IlOp::Return {
            loc: loc(),
            ret_words: 1,
        },
    ];
    dest_prop(&mut ops, 3);
    assert!(
        matches!(
            ops[5],
            IlOp::BinSlotImm {
                slot: 1,
                imm: 1,
                ..
            }
        ),
        "BinSlotImm should read src after SetField"
    );
}

#[test]
fn dest_prop_clears_across_host_invoke() {
    let mut ops = vec![
        load(0),
        store(2),
        IlOp::HostInvoke {
            arity: 0,
            layout: 0,
            loc: loc(),
        },
        load(2),
        IlOp::Return {
            loc: loc(),
            ret_words: 1,
        },
    ];
    dest_prop(&mut ops, 3);
    assert!(matches!(ops[3], IlOp::Load { slot: 2, .. }));
}

#[test]
fn dest_prop_invalidates_when_src_is_stored() {
    let mut ops = vec![
        load(0),
        store(2),
        IlOp::GetField { loc: loc() },
        IlOp::Const {
            imm: 9,
            loc: loc(),
        },
        store(0),
        load(2),
        IlOp::Return {
            loc: loc(),
            ret_words: 1,
        },
    ];
    dest_prop(&mut ops, 3);
    assert!(
        matches!(ops[5], IlOp::Load { slot: 2, .. }),
        "storing src must kill dest alias"
    );
}

#[test]
fn dest_prop_refuses_control_flow_boundaries() {
    let mut ops = vec![
        load(0),
        store(2),
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: loc(),
            hint: Default::default(),
        },
        IlOp::Label(Label(0)),
        load(2),
        IlOp::Return {
            loc: loc(),
            ret_words: 1,
        },
    ];
    dest_prop(&mut ops, 3);
    assert!(matches!(ops[4], IlOp::Load { slot: 2, .. }));
}

#[test]
fn dest_prop_does_not_clone_const_before_getfield() {
    let mut ops = vec![
        IlOp::Const {
            imm: 7,
            loc: loc(),
        },
        store(1),
        load(1),
        IlOp::GetField { loc: loc() },
        IlOp::Return {
            loc: loc(),
            ret_words: 1,
        },
    ];
    dest_prop(&mut ops, 3);
    assert!(
        matches!(ops[2], IlOp::Load { slot: 1, .. }),
        "Const clones stay with copy_prop / shape-sensitive GetField"
    );
}

#[test]
fn dest_prop_forwards_what_copy_prop_leaves_before_getfield() {
    let loc = loc();
    let mut ops = vec![
        load(0),
        store(2),
        load(2),
        IlOp::GetField { loc },
        load(2),
        IlOp::Return {
            loc,
            ret_words: 1,
        },
    ];
    copy_prop(&mut ops, 3);
    assert!(
        matches!(ops[2], IlOp::Load { slot: 2, .. }),
        "copy_prop must still refuse GetField-shaped loads"
    );
    dest_prop(&mut ops, 3);
    assert!(matches!(ops[2], IlOp::Load { slot: 0, .. }));
    assert!(matches!(ops[4], IlOp::Load { slot: 0, .. }));
}

#[test]
fn dest_prop_then_dead_store_drops_unread_copy() {
    let mut ops = vec![
        IlOp::Const {
            imm: 1,
            loc: loc(),
        },
        IlOp::Const {
            imm: 2,
            loc: loc(),
        },
        load(0),
        store(3),
        load(3),
        IlOp::MakeEnum {
            tag: 1,
            arity: 1,
            loc: loc(),
        },
        IlOp::Return {
            loc: loc(),
            ret_words: 1,
        },
    ];
    dest_prop(&mut ops, 4);
    dead_store_at(&mut ops, 4);
    assert!(
        !ops.iter()
            .any(|op| matches!(op, IlOp::StorePop { slot: 3, .. })),
        "unread dest copy should drop when tell allows"
    );
    assert!(ops.iter().any(|op| matches!(op, IlOp::Load { slot: 0, .. })));
}

#[test]
fn dest_prop_clears_across_call_byte() {
    let mut ops = vec![
        load(0),
        store(2),
        IlOp::Byte {
            byte: Byte::new(Instruction::CALL).with_operand_u32(0),
            loc: loc(),
        },
        load(2),
        IlOp::Return {
            loc: loc(),
            ret_words: 1,
        },
    ];
    dest_prop(&mut ops, 3);
    assert!(matches!(ops[3], IlOp::Load { slot: 2, .. }));
}
