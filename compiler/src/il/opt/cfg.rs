//! IL optimization — cfg passes.

use std::collections::{HashMap, HashSet};

use crate::il::op::{IlJumpKind, IlOp, Label};
use common::Instruction;

pub(super) fn label_targets(ops: &[IlOp]) -> HashMap<u32, usize> {
    let mut map = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let Some(id) = op.bind_label() {
            map.insert(id.0, i);
        }
    }
    map
}

/// Follow `JMP L` / `JMPF L` / `JMPT L` / `JumpIfMatch L` through empty
/// trampoline blocks (`Label`; `JMP L2`). One production pass collapses the
/// whole chain (cycle-safe).
///
/// Refuses `JoinLabel` trampolines so value-join / pair-`?` barriers stay put.
pub(super) fn jump_thread(ops: &mut Vec<IlOp>) -> usize {
    let targets = label_targets(ops);
    let mut hits = 0usize;
    for i in 0..ops.len() {
        let IlOp::Jump {
            kind,
            target,
            loc,
            hint,
        } = ops[i]
        else {
            continue;
        };
        let resolved = resolve_trampoline(ops, &targets, target.0);
        if resolved == target.0 {
            continue;
        }
        ops[i] = IlOp::Jump {
            kind,
            target: Label(resolved),
            loc,
            hint,
        };
        hits += 1;
    }
    hits
}

fn resolve_trampoline(ops: &[IlOp], targets: &HashMap<u32, usize>, start: u32) -> u32 {
    let mut cur = start;
    let mut seen = HashSet::new();
    for _ in 0..32 {
        if !seen.insert(cur) {
            return start;
        }
        let Some(&idx) = targets.get(&cur) else {
            return cur;
        };
        if matches!(ops.get(idx), Some(IlOp::JoinLabel(_))) {
            return cur;
        }
        let j = skip_plain_labels(ops, idx);
        match ops.get(j) {
            Some(IlOp::JoinLabel(_)) => return cur,
            Some(IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target,
                ..
            }) => {
                cur = target.0;
            }
            _ => return cur,
        }
    }
    start
}

fn skip_plain_labels(ops: &[IlOp], mut j: usize) -> usize {
    while j < ops.len() && matches!(ops[j], IlOp::Label(_)) {
        j += 1;
    }
    j
}

/// Static CFG cleanup: fall-through `JMP`, tautological `JMPF`/`JMPT` → `POP`,
/// unused plain labels. Does not drop `JoinLabel` or retarget hinted fail
/// epilogues onto other edges.
pub(super) fn simplify_cfg(ops: &mut Vec<IlOp>) -> usize {
    if ops.is_empty() {
        return 0;
    }
    let mut hits = 0usize;
    for _ in 0..8 {
        let n = simplify_cfg_once(ops);
        if n == 0 {
            break;
        }
        hits += n;
    }
    hits
}

fn simplify_cfg_once(ops: &mut Vec<IlOp>) -> usize {
    let mut hits = 0usize;
    hits += fold_fallthrough_jumps(ops);
    hits += drop_unused_plain_labels(ops);
    hits
}

fn fold_fallthrough_jumps(ops: &mut Vec<IlOp>) -> usize {
    let mut out = Vec::with_capacity(ops.len());
    let mut i = 0;
    let mut hits = 0usize;
    while i < ops.len() {
        if let IlOp::Jump {
            kind,
            target,
            loc,
            hint: _,
        } = ops[i]
        {
            if binds_soon(ops, i + 1, target.0) {
                match kind {
                    IlJumpKind::Unconditional => {
                        hits += 1;
                        i += 1;
                        continue;
                    }
                    IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue => {
                        out.push(IlOp::Pop { loc });
                        hits += 1;
                        i += 1;
                        continue;
                    }
                    IlJumpKind::JumpIfMatch { .. } => {}
                }
            }
        }
        out.push(ops[i].clone());
        i += 1;
    }
    if hits > 0 {
        *ops = out;
    }
    hits
}

/// True when `target` is bound by the run of labels starting at `from`.
fn binds_soon(ops: &[IlOp], from: usize, target: u32) -> bool {
    for op in ops.get(from..).into_iter().flatten() {
        match op {
            IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) if *id == target => return true,
            IlOp::Label(_) | IlOp::JoinLabel(_) => continue,
            _ => return false,
        }
    }
    false
}

fn drop_unused_plain_labels(ops: &mut Vec<IlOp>) -> usize {
    let used = referenced_labels(ops);
    let before = ops.len();
    ops.retain(|op| match op {
        IlOp::Label(Label(id)) => used.contains(id),
        _ => true,
    });
    before - ops.len()
}

fn referenced_labels(ops: &[IlOp]) -> HashSet<u32> {
    let mut used = HashSet::new();
    for op in ops {
        match op {
            IlOp::Jump { target, .. } | IlOp::Entry { target, .. } => {
                used.insert(target.0);
            }
            _ => {}
        }
    }
    used
}

/// `JMPF A; JMP B; A:` → `JMPT B`, dropping the trailing unconditional jump.
///
/// This is the shape every `if cond { break / return / continue }` guard emits.
/// Fusable producers invert too: fuse-select emits the `*Jmpt` twin (COI-87).
pub(crate) fn invert_branch_over_jump(ops: &mut Vec<IlOp>) {
    let mut remove: HashSet<usize> = HashSet::new();
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
    binds_soon(ops, from, target.0)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::FuseHint;
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    fn jmp(kind: IlJumpKind, id: u32) -> IlOp {
        IlOp::Jump {
            kind,
            target: Label(id),
            loc: loc(),
            hint: Default::default(),
        }
    }

    fn jmp_hinted(kind: IlJumpKind, id: u32, hint: FuseHint) -> IlOp {
        IlOp::Jump {
            kind,
            target: Label(id),
            loc: loc(),
            hint,
        }
    }

    fn label(id: u32) -> IlOp {
        IlOp::Label(Label(id))
    }

    fn ret() -> IlOp {
        IlOp::Return {
            loc: loc(),
            ret_words: 1,
        }
    }

    fn c(n: i32) -> IlOp {
        IlOp::Const { imm: n, loc: loc() }
    }

    #[test]
    fn threads_uncond_chain_in_one_pass() {
        let mut ops = vec![
            jmp(IlJumpKind::Unconditional, 1),
            label(1),
            jmp(IlJumpKind::Unconditional, 2),
            label(2),
            jmp(IlJumpKind::Unconditional, 3),
            label(3),
            ret(),
        ];
        assert_eq!(jump_thread(&mut ops), 2);
        assert!(matches!(
            ops[0],
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(3),
                ..
            }
        ));
    }

    #[test]
    fn threads_cond_jump_through_trampoline() {
        let mut ops = vec![
            c(1),
            jmp(IlJumpKind::JumpIfFalse, 1),
            c(9),
            ret(),
            label(1),
            jmp(IlJumpKind::Unconditional, 2),
            label(2),
            c(0),
            ret(),
        ];
        assert!(jump_thread(&mut ops) >= 1);
        assert!(matches!(
            ops[1],
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(2),
                ..
            }
        ));
    }

    #[test]
    fn threads_keeps_value_under_jmp_hint() {
        let hint = FuseHint::nofuse_value_under_jmp();
        let mut ops = vec![
            c(0),
            jmp_hinted(IlJumpKind::JumpIfTrue, 1, hint),
            c(1),
            ret(),
            label(1),
            jmp(IlJumpKind::Unconditional, 2),
            label(2),
            c(2),
            ret(),
        ];
        jump_thread(&mut ops);
        match &ops[1] {
            IlOp::Jump {
                kind: IlJumpKind::JumpIfTrue,
                target: Label(2),
                hint: h,
                ..
            } => assert_eq!(*h, hint),
            _ => panic!("expected hinted JMPT L2"),
        }
    }

    #[test]
    fn refuses_join_label_trampoline() {
        let mut ops = vec![
            jmp(IlJumpKind::Unconditional, 1),
            IlOp::JoinLabel(Label(1)),
            jmp(IlJumpKind::Unconditional, 2),
            label(2),
            ret(),
        ];
        let before = ops.clone();
        assert_eq!(jump_thread(&mut ops), 0);
        assert!(ops == before);
    }

    #[test]
    fn cycle_trampoline_is_a_no_op() {
        let mut ops = vec![
            jmp(IlJumpKind::Unconditional, 1),
            label(1),
            jmp(IlJumpKind::Unconditional, 2),
            label(2),
            jmp(IlJumpKind::Unconditional, 1),
        ];
        let before = ops.clone();
        assert_eq!(jump_thread(&mut ops), 0);
        assert!(ops == before);
    }

    #[test]
    fn simplify_drops_jmp_to_next_label() {
        let mut ops = vec![c(1), jmp(IlJumpKind::Unconditional, 1), label(1), ret()];
        assert!(simplify_cfg(&mut ops) >= 1);
        assert!(matches!(ops[0], IlOp::Const { imm: 1, .. }));
        assert!(matches!(ops[1], IlOp::Label(Label(1)) | IlOp::Return { .. }));
        assert!(!ops.iter().any(|op| matches!(
            op,
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                ..
            }
        )));
    }

    #[test]
    fn simplify_const_edge_jmpf_to_next_becomes_pop() {
        let mut ops = vec![
            jmp(IlJumpKind::JumpIfFalse, 1),
            label(1),
            c(3),
            ret(),
        ];
        assert!(simplify_cfg(&mut ops) >= 1);
        assert!(matches!(ops[0], IlOp::Pop { .. }));
        assert!(!ops.iter().any(|op| matches!(op, IlOp::Jump { .. })));
    }

    #[test]
    fn simplify_does_not_pop_jump_if_match_to_next() {
        let mut ops = vec![
            jmp(
                IlJumpKind::JumpIfMatch { tag: 0, arity: 1 },
                1,
            ),
            label(1),
            ret(),
        ];
        let before = ops.clone();
        simplify_cfg(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn simplify_drops_unused_plain_label_keeps_join() {
        let mut ops = vec![
            c(1),
            label(9),
            IlOp::JoinLabel(Label(8)),
            ret(),
        ];
        assert!(simplify_cfg(&mut ops) >= 1);
        assert!(!ops.iter().any(|op| matches!(op, IlOp::Label(Label(9)))));
        assert!(ops
            .iter()
            .any(|op| matches!(op, IlOp::JoinLabel(Label(8)))));
    }

    #[test]
    fn simplify_refuses_to_steal_hinted_fail_into_fallthrough() {
        let hint = FuseHint::nofuse_value_under_jmp();
        let mut ops = vec![
            c(0),
            jmp_hinted(IlJumpKind::JumpIfTrue, 1, hint),
            c(4),
            ret(),
            label(1),
            c(5),
            ret(),
        ];
        let before = ops.clone();
        simplify_cfg(&mut ops);
        assert!(ops == before);
    }
}
