//! IL optimization — cfg passes.

use crate::il::op::{IlJumpKind, IlOp};
use common::Instruction;

pub(super) fn label_targets(ops: &[IlOp]) -> std::collections::HashMap<u32, usize> {
    let mut map = std::collections::HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let Some(id) = op.bind_label() {
            map.insert(id.0, i);
        }
    }
    map
}

pub(super) fn jump_thread(ops: &mut [IlOp]) {
    let targets = label_targets(ops);
    for i in 0..ops.len() {
        let IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target,
            loc,
            hint,
        } = ops[i]
        else {
            continue;
        };
        let Some(&idx) = targets.get(&target.0) else {
            continue;
        };
        let mut j = idx;
        while j < ops.len() {
            match &ops[j] {
                IlOp::Label(_) | IlOp::JoinLabel(_) => j += 1,
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: t2,
                    ..
                } => {
                    ops[i] = IlOp::Jump {
                        kind: IlJumpKind::Unconditional,
                        target: *t2,
                        loc,
                        hint,
                    };
                    break;
                }
                _ => break,
            }
        }
    }
}

pub(super) fn is_unconditional_jmp(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            ..
        }
    )
}

pub(super) fn is_return_terminator(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Return { .. }
            | IlOp::Halt { .. }
            | IlOp::LoadReturnSlot { .. }
            | IlOp::ConstReturnImm { .. }
            | IlOp::BinReturn { .. }
    ) || matches!(
        op.as_encode_byte(),
        Some(b) if matches!(
            *b.bytecode(),
            Instruction::RETURN
                | Instruction::ReturnPair
                | Instruction::HALT
                | Instruction::LoadReturnSlot
                | Instruction::ConstReturnImm
                | Instruction::BinReturn
                | Instruction::MakeEnumReturn
                | Instruction::MakeEnumReturnK
        )
    )
}

pub(super) fn eliminate_dead_blocks(ops: &mut Vec<IlOp>) {
    let mut out = Vec::with_capacity(ops.len());
    let mut reachable = true;
    for op in ops.drain(..) {
        if matches!(op, IlOp::Label(_) | IlOp::JoinLabel(_)) {
            reachable = true;
            out.push(op);
            continue;
        }
        if !reachable {
            continue;
        }
        // Sweep after JMP and RETURN/HALT/*Return. Entry labels + CALL-0
        // continuations must be labeled so live code is not treated as
        // fall-through-after-terminator.
        let term = is_unconditional_jmp(&op) || is_return_terminator(&op);
        out.push(op);
        if term {
            reachable = false;
        }
    }
    *ops = out;
}
