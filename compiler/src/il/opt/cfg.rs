//! IL optimization — cfg passes.

use crate::il::op::{IlJumpKind, IlOp, Label};
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

pub(super) fn jump_thread(ops: &mut Vec<IlOp>) {
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

/// `JMPF A; JMP B; A:` → `JMPT B`, dropping the trailing unconditional jump.
///
/// This is the shape every `if cond { break / return / continue }` guard emits.
/// Fusable producers invert too: fuse-select emits the `*Jmpt` twin (COI-87).
pub(crate) fn invert_branch_over_jump(ops: &mut Vec<IlOp>) {
    let mut remove: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut i = 0;
    while i + 2 < ops.len() {
        let (
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: skip,
                loc,
                hint,
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: far,
                ..
            },
        ) = (&ops[i], &ops[i + 1])
        else {
            i += 1;
            continue;
        };
        if hint.blocks_cmp_jmp_fuse() {
            i += 1;
            continue;
        }
        let (skip, far, loc, hint) = (*skip, *far, *loc, *hint);
        if !labels_bind_at(ops, i + 2, skip) {
            i += 1;
            continue;
        }
        ops[i] = IlOp::Jump {
            kind: IlJumpKind::JumpIfTrue,
            target: far,
            loc,
            hint,
        };
        remove.insert(i + 1);
        i += 2;
    }
    if remove.is_empty() {
        return;
    }
    let mut out = Vec::with_capacity(ops.len());
    for (idx, op) in ops.iter().enumerate() {
        if !remove.contains(&idx) {
            out.push(op.clone());
        }
    }
    *ops = out;
}

/// True when `target` is bound by the run of labels starting at `from`, i.e. the
/// JMPF's false path is exactly the next instruction.
fn labels_bind_at(ops: &[IlOp], from: usize, target: Label) -> bool {
    for op in &ops[from..] {
        match op {
            IlOp::Label(l) | IlOp::JoinLabel(l) if *l == target => return true,
            IlOp::Label(_) | IlOp::JoinLabel(_) => continue,
            _ => return false,
        }
    }
    false
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
