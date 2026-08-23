//! Move jump-only cold blocks to the end of the buffer (COI-129).
//!
//! Only blocks that are **not** fall-through successors are relocated, so
//! existing fall-through edges stay adjacent and we never rewrite branch
//! polarity. Labels are unchanged; jumps still name the same ids.

use super::super::op::{IlJumpKind, IlOp, Label};
use super::branch_opt::BranchProfile;

#[derive(Clone, Debug)]
pub struct BlockGraph {
    pub blocks: Vec<(usize, usize)>,
    pub succs: Vec<Vec<usize>>,
}

/// Split `ops` into basic blocks. Leaders: offset 0, every label, and the
/// instruction after a jump or terminator.
pub fn build_block_graph(ops: &[IlOp]) -> BlockGraph {
    let n = ops.len();
    if n == 0 {
        return BlockGraph {
            blocks: Vec::new(),
            succs: Vec::new(),
        };
    }
    let mut leaders = vec![false; n];
    leaders[0] = true;
    for (i, op) in ops.iter().enumerate() {
        if matches!(op, IlOp::Label(_)) {
            leaders[i] = true;
        }
        if ends_block(op) && i + 1 < n {
            leaders[i + 1] = true;
        }
    }
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < n {
        if !leaders[i] {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < n && !leaders[i] {
            i += 1;
        }
        blocks.push((start, i));
    }
    let label_block = label_to_block(ops, &blocks);
    let succs: Vec<Vec<usize>> = (0..blocks.len())
        .map(|bi| successors(ops, &blocks, bi, &label_block))
        .collect();
    BlockGraph { blocks, succs }
}

/// Entry first, then original order of non-cold blocks, then detached
/// terminating blocks. `profile` can keep a would-be-cold block in place
/// when incoming jumps look hot.
pub fn compute_block_order(graph: &BlockGraph, ops: &[IlOp], profile: Option<&BranchProfile>) -> Vec<usize> {
    let n = graph.blocks.len();
    if n == 0 {
        return Vec::new();
    }
    let falls = fallthrough_flags(ops, &graph.blocks);
    let mut keep = Vec::new();
    let mut cold = Vec::new();
    for i in 0..n {
        if i == 0 {
            keep.push(i);
            continue;
        }
        let detached = !falls[i - 1];
        if detached && is_cold_block(ops, graph.blocks[i], i, graph, profile) {
            cold.push(i);
        } else {
            keep.push(i);
        }
    }
    keep.extend(cold);
    keep
}

pub fn reorder_ops(ops: &[IlOp], graph: &BlockGraph, order: &[usize]) -> Vec<IlOp> {
    let mut out = Vec::with_capacity(ops.len());
    for &i in order {
        let (s, e) = graph.blocks[i];
        out.extend_from_slice(&ops[s..e]);
    }
    out
}

/// Relayout `ops`. No-op when the order is already canonical.
/// Returns how many blocks moved from their original index.
pub fn reorder_basic_blocks(ops: &mut Vec<IlOp>, profile: Option<&BranchProfile>) -> usize {
    let graph = build_block_graph(ops);
    let order = compute_block_order(&graph, ops, profile);
    if order.windows(2).all(|w| w[1] == w[0] + 1) && order.first() == Some(&0) {
        return 0;
    }
    if order.len() != graph.blocks.len() {
        return 0;
    }
    let moved = order
        .iter()
        .enumerate()
        .filter(|(i, b)| *i != **b)
        .count();
    *ops = reorder_ops(ops, &graph, &order);
    moved
}

fn fallthrough_flags(ops: &[IlOp], blocks: &[(usize, usize)]) -> Vec<bool> {
    blocks
        .iter()
        .map(|&(s, e)| {
            if s >= e {
                return false;
            }
            can_fall_through(&ops[e - 1])
        })
        .collect()
}

fn is_cold_block(
    ops: &[IlOp],
    range: (usize, usize),
    idx: usize,
    graph: &BlockGraph,
    profile: Option<&BranchProfile>,
) -> bool {
    let (s, e) = range;
    if s >= e {
        return false;
    }
    if !is_terminator(&ops[e - 1]) {
        return false;
    }
    if graph.succs[idx].iter().any(|&t| t < idx) {
        return false;
    }
    if targeted_by_uncond(ops, s, e) {
        return false;
    }
    if profile_says_hot_entry(ops, s, e, profile) {
        return false;
    }
    true
}

fn profile_says_hot_entry(
    ops: &[IlOp],
    start: usize,
    end: usize,
    profile: Option<&BranchProfile>,
) -> bool {
    let Some(p) = profile else {
        return false;
    };
    let labels: Vec<u32> = ops[start..end]
        .iter()
        .filter_map(|op| match op {
            IlOp::Label(Label(id)) => Some(*id),
            _ => None,
        })
        .collect();
    if labels.is_empty() {
        return false;
    }
    for (i, op) in ops.iter().enumerate() {
        let IlOp::Jump { target, .. } = op else {
            continue;
        };
        if !labels.contains(&target.0) {
            continue;
        }
        let taken = p.taken.get(&i).copied().unwrap_or(0);
        let not_taken = p.not_taken.get(&i).copied().unwrap_or(0);
        if taken > not_taken && taken > 0 {
            return true;
        }
    }
    false
}

fn targeted_by_uncond(ops: &[IlOp], start: usize, end: usize) -> bool {
    let labels: Vec<u32> = ops[start..end]
        .iter()
        .filter_map(|op| match op {
            IlOp::Label(Label(id)) => Some(*id),
            _ => None,
        })
        .collect();
    if labels.is_empty() {
        return false;
    }
    for op in ops {
        if let IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target,
            ..
        } = op
        {
            if labels.contains(&target.0) {
                return true;
            }
        }
    }
    false
}

fn label_to_block(ops: &[IlOp], blocks: &[(usize, usize)]) -> std::collections::HashMap<u32, usize> {
    let mut map = std::collections::HashMap::new();
    for (bi, &(s, e)) in blocks.iter().enumerate() {
        for op in &ops[s..e] {
            if let IlOp::Label(Label(id)) = op {
                map.insert(*id, bi);
            }
        }
    }
    map
}

fn successors(
    ops: &[IlOp],
    blocks: &[(usize, usize)],
    bi: usize,
    label_block: &std::collections::HashMap<u32, usize>,
) -> Vec<usize> {
    let (s, e) = blocks[bi];
    let mut succs = Vec::new();
    for op in &ops[s..e] {
        if let IlOp::Jump { target, .. } = op {
            if let Some(&t) = label_block.get(&target.0) {
                if !succs.contains(&t) {
                    succs.push(t);
                }
            }
        }
    }
    if e > s && can_fall_through(&ops[e - 1]) && bi + 1 < blocks.len() && !succs.contains(&(bi + 1))
    {
        succs.push(bi + 1);
    }
    succs
}

fn ends_block(op: &IlOp) -> bool {
    matches!(op, IlOp::Jump { .. }) || is_terminator(op)
}

fn can_fall_through(op: &IlOp) -> bool {
    !is_terminator(op)
        && !matches!(
            op,
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                ..
            }
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    fn jmpf(id: u32) -> IlOp {
        IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(id),
            loc: loc(),
        }
    }

    fn jmp(id: u32) -> IlOp {
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(id),
            loc: loc(),
        }
    }

    fn label(id: u32) -> IlOp {
        IlOp::Label(Label(id))
    }

    fn ret() -> IlOp {
        IlOp::Return { loc: loc() }
    }

    fn c(n: i32) -> IlOp {
        IlOp::Const {
            imm: n,
            loc: loc(),
        }
    }

    #[test]
    fn linear_code_unchanged() {
        let mut ops = vec![c(1), c(2), ret()];
        let before = ops.clone();
        reorder_basic_blocks(&mut ops, None);
        assert!(ops == before);
    }

    #[test]
    fn cold_return_block_moves_past_join() {
        let mut ops = vec![
            jmpf(1),
            c(2),
            jmp(2),
            label(1),
            c(0),
            ret(),
            label(2),
            c(3),
            ret(),
        ];
        reorder_basic_blocks(&mut ops, None);
        let labels: Vec<u32> = ops
            .iter()
            .filter_map(|op| match op {
                IlOp::Label(Label(id)) => Some(*id),
                _ => None,
            })
            .collect();
        assert_eq!(labels, vec![2, 1], "join (2) before cold (1)");
        assert!(matches!(ops[0], IlOp::Jump { kind: IlJumpKind::JumpIfFalse, target: Label(1), .. }));
        assert!(ops.iter().any(|op| matches!(op, IlOp::Const { imm: 3, .. })));
        let last_real = ops.iter().rev().find(|op| !matches!(op, IlOp::Label(_)));
        assert!(matches!(last_real, Some(IlOp::Return { .. })));
    }

    #[test]
    fn loop_header_stays_ahead_of_back_edge() {
        let mut ops = vec![
            label(1),
            c(1),
            jmpf(2),
            c(2),
            jmp(1),
            label(2),
            ret(),
        ];
        let header_pos = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(1))))
            .unwrap();
        reorder_basic_blocks(&mut ops, None);
        let header_after = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(1))))
            .unwrap();
        let latch = ops.iter().position(|op| {
            matches!(
                op,
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(1),
                    ..
                }
            )
        });
        assert!(header_after <= header_pos || latch.is_some());
        assert!(header_after < latch.unwrap());
    }

    #[test]
    fn branch_targets_keep_the_same_label_ids() {
        let mut ops = vec![jmpf(1), c(2), jmp(2), label(1), ret(), label(2), ret()];
        reorder_basic_blocks(&mut ops, None);
        let targets: Vec<u32> = ops
            .iter()
            .filter_map(|op| match op {
                IlOp::Jump { target, .. } => Some(target.0),
                _ => None,
            })
            .collect();
        assert!(targets.contains(&1) && targets.contains(&2));
    }

    #[test]
    fn hot_profile_keeps_cold_looking_block() {
        let mut ops = vec![
            jmpf(1),
            c(2),
            jmp(2),
            label(1),
            ret(),
            label(2),
            ret(),
        ];
        let mut profile = BranchProfile::default();
        profile.taken.insert(0, 99);
        profile.not_taken.insert(0, 1);
        let before_labels: Vec<u32> = ops
            .iter()
            .filter_map(|op| match op {
                IlOp::Label(Label(id)) => Some(*id),
                _ => None,
            })
            .collect();
        reorder_basic_blocks(&mut ops, Some(&profile));
        let after: Vec<u32> = ops
            .iter()
            .filter_map(|op| match op {
                IlOp::Label(Label(id)) => Some(*id),
                _ => None,
            })
            .collect();
        assert_eq!(after, before_labels);
    }
}
