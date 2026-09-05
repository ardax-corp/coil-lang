//! Heuristic branch layout (COI-128).
//!
//! Keeps the likely successor as fall-through. A terminating then-arm
//! after `JMPF` is treated as cold (error / early return) and moved off
//! the fall-through. Semantics stay identical: only the sense of the
//! branch and the linear order of a single-entry cold region change.

use std::collections::HashMap;

use common::Instruction;

use super::super::op::{IlJumpKind, IlOp, Label};
use super::super::sp::{self, Sp};

/// Highest label id bound or targeted by `ops` (jumps and calls), or `0`.
pub(crate) fn max_code_label(ops: &[IlOp]) -> u32 {
    ops.iter().filter_map(code_label_id).max().unwrap_or(0)
}

/// Next id that does not collide with any label or jump/call target in `ops`.
pub(crate) fn next_fresh_label(ops: &[IlOp]) -> u32 {
    max_code_label(ops).saturating_add(1)
}

fn code_label_id(op: &IlOp) -> Option<u32> {
    match op {
        IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
        IlOp::Jump { target, .. } | IlOp::Entry { target, .. } => Some(target.0),
        _ => None,
    }
}

#[inline]
fn remap_label_target(id: u32, local: &HashMap<u32, u32>, prior: &HashMap<u32, u32>) -> u32 {
    local
        .get(&id)
        .or_else(|| prior.get(&id))
        .copied()
        .unwrap_or(id)
}

/// Remap labels in `ops` into a fresh id space starting at `*next_label`.
///
/// Jump targets that refer to labels bound in earlier concatenated chunks
/// are resolved via `prior_labels`. Cross-function `Entry` (CALL/CodePtr)
/// is patched separately from each function's recorded entry label.
pub(crate) fn remap_label_space(
    ops: &[IlOp],
    next_label: &mut u32,
    prior_labels: &HashMap<u32, u32>,
) -> (Vec<IlOp>, HashMap<u32, u32>) {
    if ops.is_empty() {
        return (Vec::new(), HashMap::new());
    }
    let mut map = HashMap::<u32, u32>::new();
    for op in ops {
        if let IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) = op {
            map.entry(*id).or_insert_with(|| {
                let n = *next_label;
                *next_label = next_label.saturating_add(1);
                n
            });
        }
    }
    if map.is_empty() && prior_labels.is_empty() {
        return (ops.to_vec(), map);
    }
    let remapped = ops
        .iter()
        .map(|op| match op {
            IlOp::Label(Label(id)) => IlOp::Label(Label(map[id])),
            IlOp::JoinLabel(Label(id)) => IlOp::JoinLabel(Label(map[id])),
            IlOp::Jump {
                kind,
                target,
                loc,
                hint,
            } => IlOp::Jump {
                kind: *kind,
                target: Label(remap_label_target(target.0, &map, prior_labels)),
                loc: *loc,
                hint: *hint,
            },
            IlOp::Entry {
                kind,
                arity,
                target,
                loc,
                ret_words,
            } => IlOp::Entry {
                kind: *kind,
                arity: *arity,
                // Local recursive calls only. Cross-function CALL/CodePtr
                // targets are patched from function entry labels after concat
                // (`prior` collides when two bodies reuse 0..n).
                target: Label(map.get(&target.0).copied().unwrap_or(target.0)),
                loc: *loc,
                ret_words: *ret_words,
            },
            other => other.clone(),
        })
        .collect();
    (remapped, map)
}

/// Reorder cold terminating arms off the fall-through of `JMPF`/`JMPT`.
#[cfg(test)]
pub fn optimize_branches(ops: &mut Vec<IlOp>) {
    let mut next = next_fresh_label(ops);
    optimize_branches_at(ops, 0, &mut next);
}

/// Like [`optimize_branches`], seeding SP at `entry_sp` and minting labels
/// from `next_label` (bumped to at least [`next_fresh_label`]).
pub(crate) fn optimize_branches_at(
    ops: &mut Vec<IlOp>,
    entry_sp: i32,
    next_label: &mut u32,
) -> usize {
    *next_label = (*next_label).max(next_fresh_label(ops));
    let mut applied = 0usize;
    let mut guard = 0usize;
    while guard < ops.len() {
        guard += 1;
        if !invert_one_cold_fallthrough(ops, entry_sp, next_label) {
            break;
        }
        applied += 1;
    }
    applied
}

fn invert_one_cold_fallthrough(
    ops: &mut Vec<IlOp>,
    entry_sp: i32,
    next_label: &mut u32,
) -> bool {
    let targets = label_index(ops);
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
        if hint.blocks_cold_fallthrough_invert() {
            continue;
        }
        if !matches!(kind, IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue) {
            continue;
        }
        let Some(&lab_i) = targets.get(&target.0) else {
            continue;
        };
        if lab_i <= i + 1 {
            continue;
        }
        let cold = &ops[i + 1..lab_i];
        if cold.is_empty() || !is_cold_region(cold) {
            continue;
        }
        if region_defines_labels(cold) {
            continue;
        }
        if !suffix_cannot_fall_into_moved_cold(&ops[lab_i..]) {
            continue;
        }
        if !fallthrough_is_sp_safe(ops, i, lab_i, entry_sp) {
            continue;
        }
        *next_label = (*next_label).max(next_fresh_label(ops));
        let fresh = Label(*next_label);
        *next_label = next_label.saturating_add(1);
        let mut out = Vec::with_capacity(ops.len() + 2);
        out.extend_from_slice(&ops[..i]);
        let inverted = match kind {
            IlJumpKind::JumpIfFalse => IlJumpKind::JumpIfTrue,
            IlJumpKind::JumpIfTrue => IlJumpKind::JumpIfFalse,
            _ => unreachable!(),
        };
        out.push(IlOp::Jump {
            kind: inverted,
            target: fresh,
            loc,
            hint: Default::default(),
        });
        out.extend_from_slice(&ops[lab_i..]);
        out.push(IlOp::Label(fresh));
        out.extend_from_slice(cold);
        *ops = out;
        return true;
    }
    false
}

fn is_cold_region(ops: &[IlOp]) -> bool {
    if ops
        .iter()
        .any(|op| matches!(op, IlOp::Label(_) | IlOp::JoinLabel(_)))
    {
        return false;
    }
    if ops.iter().any(is_jump) {
        return false;
    }
    matches!(ops.last(), Some(op) if is_terminator(op))
}

fn region_defines_labels(ops: &[IlOp]) -> bool {
    ops.iter()
        .any(|op| matches!(op, IlOp::Label(_) | IlOp::JoinLabel(_)))
}

fn suffix_cannot_fall_into_moved_cold(suffix: &[IlOp]) -> bool {
    let mut last_real = None;
    for op in suffix {
        if matches!(op, IlOp::Label(_) | IlOp::JoinLabel(_)) {
            continue;
        }
        last_real = Some(op);
    }
    match last_real {
        Some(op) => is_terminator(op) || is_uncond_jmp(op),
        None => false,
    }
}

fn is_terminator(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Return { .. }
            | IlOp::Halt { .. }
            | IlOp::LoadReturnSlot { .. }
            | IlOp::ConstReturnImm { .. }
            | IlOp::BinReturn { .. }
    )
}

fn is_uncond_jmp(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            ..
        }
    )
}

fn is_jump(op: &IlOp) -> bool {
    matches!(op, IlOp::Jump { .. })
}

fn label_index(ops: &[IlOp]) -> HashMap<u32, usize> {
    let mut map = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) = op {
            map.insert(*id, i);
        }
    }
    map
}

fn fallthrough_is_sp_safe(ops: &[IlOp], jump_i: usize, lab_i: usize, entry_sp: i32) -> bool {
    let info = sp::analyze_at(ops, entry_sp);
    let Sp::Known(h) = info.sp_before(jump_i) else {
        return false;
    };
    if h < 1 {
        return false;
    }
    let after = h - 1;
    let Sp::Known(h_else) = info.sp_before(lab_i) else {
        return false;
    };
    if h_else != after {
        return false;
    }
    let mut h_arm = after;
    for op in &ops[jump_i + 1..lab_i] {
        if is_seek(op) {
            return false;
        }
        let Some(d) = sp::stack_delta(op) else {
            return false;
        };
        h_arm += d;
        if h_arm < 0 {
            return false;
        }
    }
    true
}

fn is_seek(op: &IlOp) -> bool {
    matches!(op, IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Seek)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::op::IlOp;
    use common::DebugLoc;
    use std::collections::HashSet;

    fn used_labels(ops: &[IlOp]) -> HashSet<u32> {
        let mut s = HashSet::new();
        for op in ops {
            if let IlOp::Jump { target, .. } = op {
                s.insert(target.0);
            }
        }
        s
    }

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    fn jmpf(id: u32) -> IlOp {
        IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(id),
            loc: loc(),
            hint: Default::default(),
        }
    }

    fn jmpt(id: u32) -> IlOp {
        IlOp::Jump {
            kind: IlJumpKind::JumpIfTrue,
            target: Label(id),
            loc: loc(),
            hint: Default::default(),
        }
    }

    fn label(id: u32) -> IlOp {
        IlOp::Label(Label(id))
    }

    fn ret() -> IlOp {
        IlOp::Return { loc: loc(), ret_words: 1}
    }

    fn c(n: i32) -> IlOp {
        IlOp::Const { imm: n, loc: loc() }
    }

    #[test]
    fn heuristic_moves_return_off_jmpf_fallthrough() {
        let mut ops = vec![c(0), jmpf(1), c(1), ret(), label(1), c(2), ret()];
        optimize_branches(&mut ops);
        assert!(
            matches!(
                ops[1],
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfTrue,
                    ..
                }
            ),
            "expected JMPT to cold, got jump or other",
        );
        assert!(matches!(ops[2], IlOp::Label(Label(1))));
        assert!(matches!(ops[3], IlOp::Const { imm: 2, .. }));
        assert!(matches!(ops[4], IlOp::Return { .. }));
        assert!(matches!(ops.last(), Some(IlOp::Return { .. })));
        let jumps: Vec<_> = ops
            .iter()
            .filter_map(|op| match op {
                IlOp::Jump { kind, target, .. } => Some((*kind, target.0)),
                _ => None,
            })
            .collect();
        assert_eq!(jumps.len(), 1);
        assert_eq!(jumps[0].0, IlJumpKind::JumpIfTrue);
        assert!(used_labels(&ops).contains(&jumps[0].1));
    }

    #[test]
    fn refuses_when_then_arm_has_internal_jump() {
        let mut ops = vec![
            jmpf(1),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            label(1),
            ret(),
        ];
        let before = ops.clone();
        optimize_branches(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn control_flow_still_returns_on_both_arms() {
        let mut ops = vec![c(0), jmpf(1), c(1), ret(), label(1), c(2), ret()];
        optimize_branches(&mut ops);
        let returns = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Return { .. }))
            .count();
        assert_eq!(returns, 2);
        assert!(matches!(
            ops[1],
            IlOp::Jump {
                kind: IlJumpKind::JumpIfTrue,
                ..
            }
        ));
    }

    #[test]
    fn jmpt_cold_fallthrough_inverts_to_jmpf() {
        let mut ops = vec![c(0), jmpt(1), c(1), ret(), label(1), c(2), ret()];
        optimize_branches(&mut ops);
        assert!(matches!(
            ops[1],
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                ..
            }
        ));
    }

    #[test]
    fn value_under_jmp_try_refuses_cold_invert() {
        let hinted = IlOp::jump_hinted(
            IlJumpKind::JumpIfTrue,
            Label(1),
            loc(),
            crate::il::FuseHint::nofuse_value_under_jmp(),
        );
        let mut ops = vec![c(0), hinted, c(1), ret(), label(1), c(2), ret()];
        let before = ops.clone();
        optimize_branches(&mut ops);
        assert!(
            ops == before,
            "pair-? JMPT must stay on the shared fail"
        );
    }

    #[test]
    fn refuses_when_cond_jump_has_empty_stack() {
        let mut ops = vec![jmpf(1), c(1), ret(), label(1), c(2), ret()];
        let before = ops.clone();
        optimize_branches(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn fresh_label_respects_caller_watermark() {
        let mut ops = vec![c(0), jmpf(1), c(1), ret(), label(1), c(2), ret()];
        let mut next = 40;
        optimize_branches_at(&mut ops, 0, &mut next);
        let target = ops
            .iter()
            .find_map(|op| match op {
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfTrue,
                    target,
                    ..
                } => Some(target.0),
                _ => None,
            })
            .expect("inverted JMPT");
        assert_eq!(target, 40);
        assert_eq!(next, 41);
    }
}
