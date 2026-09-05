//! Structural cold outlining / layout (COI-129 family).
//!
//! 1. Outline a `JumpIfMatch` (or hinted cond) miss that is Panic / Halt /
//!    `FuseHint::cold_miss` by inserting `JMP` and parking the miss at the
//!    end. Polarity is not rewritten.
//! 2. Sink detached terminator / Panic / `FuseHint::cold_target` blocks to
//!    the end. Fall-through successors stay adjacent except after (1).
//!
//! Pair-`?` `ValueUnderJmp` jumps are refused. Labels keep their ids.

use common::Instruction;

use super::super::op::{FuseHint, IlJumpKind, IlOp, Label};
use super::branch_opt;

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
        if matches!(op, IlOp::Label(_) | IlOp::JoinLabel(_)) {
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
/// terminating blocks.
pub fn compute_block_order(graph: &BlockGraph, ops: &[IlOp]) -> Vec<usize> {
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
        if detached && is_cold_block(ops, graph.blocks[i], i, graph) {
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
#[allow(dead_code)]
pub fn reorder_basic_blocks(ops: &mut Vec<IlOp>) -> usize {
    let mut next = branch_opt::next_fresh_label(ops);
    reorder_basic_blocks_at(ops, &mut next)
}

/// Like [`reorder_basic_blocks`], minting outline labels from `next_label`.
pub fn reorder_basic_blocks_at(ops: &mut Vec<IlOp>, next_label: &mut u32) -> usize {
    *next_label = (*next_label).max(branch_opt::next_fresh_label(ops));
    let mut outlined = 0usize;
    let mut guard = 0usize;
    while guard < ops.len() {
        guard += 1;
        if !outline_one_cold_miss(ops, next_label) {
            break;
        }
        outlined += 1;
    }
    let graph = build_block_graph(ops);
    let order = compute_block_order(&graph, ops);
    if order.windows(2).all(|w| w[1] == w[0] + 1) && order.first() == Some(&0) {
        return outlined;
    }
    if order.len() != graph.blocks.len() {
        return outlined;
    }
    let moved = order
        .iter()
        .enumerate()
        .filter(|(i, b)| *i != **b)
        .count();
    *ops = reorder_ops(ops, &graph, &order);
    outlined + moved
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
) -> bool {
    let (s, e) = range;
    if s >= e {
        return false;
    }
    if graph.succs[idx].iter().any(|&t| t < idx) {
        return false;
    }
    let last = &ops[e - 1];
    let panic_block = ops[s..e].iter().any(is_panic);
    let hinted = targeted_by_cold_hint(ops, s, e);
    let term = is_terminator(last);
    let jmp_out = is_uncond_jmp(last);
    if !term && !(hinted && jmp_out) && !panic_block {
        return false;
    }
    if targeted_by_uncond(ops, s, e) && !panic_block && !hinted {
        return false;
    }
    true
}

/// Insert `JMP` over a structural cold `JumpIfMatch` / hinted miss and
/// park the miss at the end. Refuses `ValueUnderJmp`.
fn outline_one_cold_miss(ops: &mut Vec<IlOp>, next_label: &mut u32) -> bool {
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
        if !matches!(
            kind,
            IlJumpKind::JumpIfMatch { .. } | IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue
        ) {
            continue;
        }
        // JMPF/JMPT miss outlining is COI-128 invert's job unless hinted.
        if matches!(kind, IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue) && !hint.cold_miss
        {
            continue;
        }
        let Some(&lab_i) = targets.get(&target.0) else {
            continue;
        };
        if lab_i <= i + 1 {
            continue;
        }
        let miss = &ops[i + 1..lab_i];
        if miss.is_empty() || region_defines_labels(miss) {
            continue;
        }
        // Already outlined: cond; JMP cold; L_hot:
        if miss.len() == 1 && is_uncond_jmp(&miss[0]) {
            continue;
        }
        if !should_outline_miss(hint, miss) {
            continue;
        }
        *next_label = (*next_label).max(branch_opt::next_fresh_label(ops));
        let fresh = Label(*next_label);
        *next_label = next_label.saturating_add(1);
        let mut out = Vec::with_capacity(ops.len() + 2);
        out.extend_from_slice(&ops[..=i]);
        out.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: fresh,
            loc,
            hint: FuseHint::cold_target(),
        });
        out.extend_from_slice(&ops[lab_i..]);
        out.push(IlOp::Label(fresh));
        out.extend_from_slice(miss);
        *ops = out;
        return true;
    }
    false
}

fn should_outline_miss(hint: FuseHint, miss: &[IlOp]) -> bool {
    if hint.cold_miss {
        return miss_ends_closed(miss);
    }
    miss.iter().any(is_panic) && miss_ends_closed(miss)
}

fn miss_ends_closed(miss: &[IlOp]) -> bool {
    match miss.last() {
        Some(op) => is_terminator(op) || is_uncond_jmp(op),
        None => false,
    }
}

fn region_defines_labels(ops: &[IlOp]) -> bool {
    ops.iter()
        .any(|op| matches!(op, IlOp::Label(_) | IlOp::JoinLabel(_)))
}

fn label_index(ops: &[IlOp]) -> std::collections::HashMap<u32, usize> {
    let mut map = std::collections::HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) = op {
            map.insert(*id, i);
        }
    }
    map
}

fn targeted_by_cold_hint(ops: &[IlOp], start: usize, end: usize) -> bool {
    let labels: Vec<u32> = ops[start..end]
        .iter()
        .filter_map(|op| match op {
            IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
            _ => None,
        })
        .collect();
    if labels.is_empty() {
        return false;
    }
    ops.iter().any(|op| match op {
        IlOp::Jump { target, hint, .. } if hint.cold_target => labels.contains(&target.0),
        _ => false,
    })
}

fn targeted_by_uncond(ops: &[IlOp], start: usize, end: usize) -> bool {
    let labels: Vec<u32> = ops[start..end]
        .iter()
        .filter_map(|op| match op {
            IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
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
            if let IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) = op {
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

fn is_uncond_jmp(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            ..
        }
    )
}

fn is_panic(op: &IlOp) -> bool {
    matches!(
        op.as_encode_byte(),
        Some(b) if *b.bytecode() == Instruction::Panic
    ) || matches!(
        op,
        IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Panic
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
    ) || is_panic(op)
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
            hint: Default::default(),
        }
    }

    fn jmp(id: u32) -> IlOp {
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
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
        IlOp::Const {
            imm: n,
            loc: loc(),
        }
    }

    #[test]
    fn linear_code_unchanged() {
        let mut ops = vec![c(1), c(2), ret()];
        let before = ops.clone();
        reorder_basic_blocks(&mut ops);
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
        reorder_basic_blocks(&mut ops);
        let labels: Vec<u32> = ops
            .iter()
            .filter_map(|op| match op {
                IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
                _ => None,
            })
            .collect();
        assert_eq!(labels, vec![2, 1], "join (2) before cold (1)");
        assert!(matches!(ops[0], IlOp::Jump { kind: IlJumpKind::JumpIfFalse, target: Label(1), .. }));
        assert!(ops.iter().any(|op| matches!(op, IlOp::Const { imm: 3, .. })));
        let last_real = ops.iter().rev().find(|op| !matches!(op, IlOp::Label(_) | IlOp::JoinLabel(_)));
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
        reorder_basic_blocks(&mut ops);
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
        reorder_basic_blocks(&mut ops);
        let targets: Vec<u32> = ops
            .iter()
            .filter_map(|op| match op {
                IlOp::Jump { target, .. } => Some(target.0),
                _ => None,
            })
            .collect();
        assert!(targets.contains(&1) && targets.contains(&2));
    }

    fn jim(tag: u32, id: u32) -> IlOp {
        IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { tag, arity: 0 },
            target: Label(id),
            loc: loc(),
            hint: Default::default(),
        }
    }

    fn panic_op() -> IlOp {
        IlOp::Byte {
            byte: common::Byte::new(Instruction::Panic),
            loc: loc(),
        }
    }

    #[test]
    fn jump_if_match_panic_miss_is_outlined() {
        let mut ops = vec![jim(0, 1), panic_op(), label(1), c(2), ret()];
        reorder_basic_blocks(&mut ops);
        assert!(
            matches!(
                ops[0],
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfMatch { tag: 0, .. },
                    target: Label(1),
                    ..
                }
            ),
            "JumpIfMatch still targets the hot arm"
        );
        assert!(
            matches!(
                ops[1],
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    ..
                }
            ),
            "miss becomes JMP to outlined Panic"
        );
        assert!(matches!(ops[2], IlOp::Label(Label(1))));
        let last = ops
            .iter()
            .rev()
            .find(|op| !matches!(op, IlOp::Label(_) | IlOp::JoinLabel(_)));
        assert!(last.is_some_and(is_panic));
    }

    #[test]
    fn value_under_jmp_try_refuses_outline() {
        let hinted = IlOp::jump_hinted(
            IlJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            Label(1),
            loc(),
            FuseHint::nofuse_value_under_jmp(),
        );
        let mut ops = vec![hinted, panic_op(), label(1), c(2), ret()];
        let before = ops.clone();
        reorder_basic_blocks(&mut ops);
        assert!(ops == before, "pair-? JumpIfMatch miss must stay put");
    }

    #[test]
    fn cold_miss_hint_outlines_err_arm() {
        let hinted = IlOp::jump_hinted(
            IlJumpKind::JumpIfMatch { tag: 0, arity: 0 },
            Label(1),
            loc(),
            FuseHint::cold_miss(),
        );
        let mut ops = vec![hinted, c(-1), jmp(2), label(1), c(3), jmp(2), label(2), ret()];
        reorder_basic_blocks(&mut ops);
        assert!(matches!(
            ops[1],
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                ..
            }
        ));
        let consts: Vec<i32> = ops
            .iter()
            .filter_map(|op| match op {
                IlOp::Const { imm, .. } => Some(*imm),
                _ => None,
            })
            .collect();
        assert_eq!(consts, vec![3, -1], "hot Ok const before outlined Err");
    }

    #[test]
    fn cold_target_sinks_detached_err_block() {
        let to_err = IlOp::jump_hinted(
            IlJumpKind::JumpIfMatch { tag: 1, arity: 0 },
            Label(1),
            loc(),
            FuseHint::cold_target(),
        );
        let mut ops = vec![
            to_err,
            c(2),
            jmp(2),
            label(1),
            c(0),
            jmp(2),
            label(2),
            ret(),
        ];
        reorder_basic_blocks(&mut ops);
        let labels: Vec<u32> = ops
            .iter()
            .filter_map(|op| match op {
                IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
                _ => None,
            })
            .collect();
        assert_eq!(labels.last().copied(), Some(1), "Err target sinks last");
    }

    #[test]
    fn detached_panic_block_sinks() {
        let mut ops = vec![jmpf(1), c(2), jmp(2), label(1), panic_op(), label(2), ret()];
        reorder_basic_blocks(&mut ops);
        let last = ops
            .iter()
            .rev()
            .find(|op| !matches!(op, IlOp::Label(_) | IlOp::JoinLabel(_)));
        assert!(last.is_some_and(is_panic));
    }
}
