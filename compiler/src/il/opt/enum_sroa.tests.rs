use super::*;
use crate::il::op::EntryKind;
use common::{Byte, DebugLoc};

fn loc() -> DebugLoc {
    DebugLoc::unknown()
}

fn load(slot: u32) -> IlOp {
    IlOp::Load { slot, loc: loc() }
}

fn store(slot: u32) -> IlOp {
    IlOp::StorePop { slot, loc: loc() }
}

fn konst(imm: i32) -> IlOp {
    IlOp::Const { imm, loc: loc() }
}

fn seek(slot: u32) -> IlOp {
    IlOp::byte(Byte::new(Instruction::Seek).with_operand_u32(slot))
}

fn unpack(n: u32) -> IlOp {
    IlOp::byte(Byte::new(Instruction::Unpack).with_operand_u32(n))
}

fn jim(tag: u32, arity: u32, target: u32) -> IlOp {
    IlOp::jump(IlJumpKind::JumpIfMatch { tag, arity }, Label(target), loc())
}

fn make(tag: u16, arity: u16) -> IlOp {
    IlOp::MakeEnum {
        kinds: 0,
        tag,
        arity,
        loc: loc(),
    }
}

fn ret() -> IlOp {
    IlOp::Return {
        ret_words: 1,
        loc: loc(),
    }
}

/// `fn(int x) { let s = Rect(x, x + 1); return match s { Circle(r) => r,
/// Rect(w, h) => w + h }; }` as match codegen shapes it (params: slot 0).
fn rect_match() -> Vec<IlOp> {
    vec![
        load(0),
        konst(1),
        IlOp::Bin {
            op: Instruction::ADD,
            loc: loc(),
        },
        load(0),
        make(1, 2),
        store(1),
        seek(2),
        load(1),
        jim(0, 1, 10),
        unpack(2),
        load(2),
        load(3),
        IlOp::Bin {
            op: Instruction::ADD,
            loc: loc(),
        },
        ret(),
        IlOp::Label(Label(10)),
        load(2),
        ret(),
    ]
}

fn has_make_enum(ops: &[IlOp]) -> bool {
    ops.iter().any(|op| matches!(op, IlOp::MakeEnum { .. }))
}

#[test]
fn private_enum_becomes_tag_and_payload_slots() {
    let mut ops = rect_match();
    let mut next = 100;
    assert_eq!(scalarize_enums(&mut ops, 1, &mut next), 1);
    assert!(!has_make_enum(&ops));
    assert!(!ops.iter().any(|op| op.as_plain_byte().is_some_and(|b| {
        matches!(*b.bytecode(), Instruction::Unpack)
    })));
    // Tags never built here (Circle) drop their dispatch arm.
    assert!(!ops.iter().any(|op| matches!(op, IlOp::Jump { .. })));
    // Payload lands where the scrutinee sat: slots 2 and 3.
    assert!(ops.contains(&store(2)) && ops.contains(&store(3)));
}

#[test]
fn drop_enum_stays_on_the_heap() {
    let mut ops = rect_match();
    ops.insert(
        5,
        IlOp::byte(Byte::new(Instruction::TagEnumType).with_operand_u32(1)),
    );
    let mut next = 100;
    assert_eq!(scalarize_enums(&mut ops, 1, &mut next), 0);
    assert!(has_make_enum(&ops));
}

#[test]
fn whole_value_use_keeps_the_heap_enum() {
    let mut ops = rect_match();
    // Pass `s` to a call before matching it.
    ops.splice(
        6..6,
        [
            load(1),
            IlOp::Entry {
                kind: EntryKind::Call,
                arity: 1,
                target: Label(50),
                ret_words: 1,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
        ],
    );
    let mut next = 100;
    assert_eq!(scalarize_enums(&mut ops, 1, &mut next), 0);
}

#[test]
fn seek_while_live_keeps_the_heap_enum() {
    // Match `s` twice: the first match's `Seek` exposes the fresh slots
    // while the second still needs them.
    let mut ops = rect_match();
    let first_arm = vec![
        seek(2),
        load(1),
        unpack(2),
        store(4),
        store(5),
    ];
    ops.splice(6..6, first_arm);
    let mut next = 100;
    assert_eq!(scalarize_enums(&mut ops, 1, &mut next), 0);
}
