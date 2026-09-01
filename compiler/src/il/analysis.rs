//! Shared CFG / loop / slot-liveness views over a `Vec<IlOp>`.
//!
//! Not an IR: passes still rewrite the op buffer in place. One natural-loop
//! finder and one block/liveness implementation replace the copies that used
//! to live in licm, bounds, unroll, invariant_store_elim, and slot_promote.

use std::collections::{HashMap, HashSet};

use common::Instruction;

use super::op::{IlJumpKind, IlOp, Label};

/// Natural loop identified by an unconditional back-edge to a header label.
#[derive(Clone, Debug)]
pub(crate) struct NaturalLoop {
    pub(crate) header: usize,
    /// Index of back-edge `Jump` (unconditional) to header.
    pub(crate) latch: usize,
    pub(crate) header_label: Label,
}

impl NaturalLoop {
    pub(crate) fn body_start(&self) -> usize {
        self.header + 1
    }
}

/// IL is module-flat; labels reuse per function. Scope lookups to the function
/// containing `idx` (ops since the previous `Return`).
pub(crate) fn il_function_start(ops: &[IlOp], idx: usize) -> usize {
    for i in (0..idx).rev() {
        if matches!(ops[i], IlOp::Return { .. }) {
            return i + 1;
        }
    }
    0
}

fn resolve_label_before(ops: &[IlOp], before: usize, target: Label) -> Option<usize> {
    let start = il_function_start(ops, before);
    for i in (start..before).rev() {
        if matches!(&ops[i], IlOp::Label(l) | IlOp::JoinLabel(l) if *l == target) {
            return Some(i);
        }
    }
    None
}

/// Unconditional back-edges whose target label binds earlier in the same function.
pub(crate) fn find_natural_loops(ops: &[IlOp]) -> Vec<NaturalLoop> {
    let mut out = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        let IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target,
            ..
        } = op
        else {
            continue;
        };
        let Some(h) = resolve_label_before(ops, i, *target) else {
            continue;
        };
        if h >= i {
            continue;
        }
        out.push(NaturalLoop {
            header: h,
            latch: i,
            header_label: *target,
        });
    }
    out
}

/// Straight-line region between leaders (labels / jump targets / fall-through
/// after terminators). `JoinLabel` is not a leader — same as slot_promote.
#[derive(Clone, Debug)]
pub(crate) struct Block {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) succs: Vec<usize>,
}

pub(crate) fn build_blocks(ops: &[IlOp]) -> Vec<Block> {
    if ops.is_empty() {
        return Vec::new();
    }
    let mut leaders: HashSet<usize> = HashSet::new();
    leaders.insert(0);
    let mut label_at: HashMap<u32, usize> = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let IlOp::Label(Label(id)) = op {
            label_at.insert(*id, i);
            leaders.insert(i);
        }
    }
    for (i, op) in ops.iter().enumerate() {
        if let IlOp::Jump { target, .. } = op {
            if let Some(&t) = label_at.get(&target.0) {
                leaders.insert(t);
            }
            if i + 1 < ops.len() {
                leaders.insert(i + 1);
            }
        } else if matches!(
            op,
            IlOp::Return { .. }
                | IlOp::Halt { .. }
                | IlOp::LoadReturnSlot { .. }
                | IlOp::ConstReturnImm { .. }
                | IlOp::BinReturn { .. }
        ) && i + 1 < ops.len()
        {
            leaders.insert(i + 1);
        }
    }
    let mut starts: Vec<usize> = leaders.into_iter().collect();
    starts.sort_unstable();
    let mut blocks: Vec<Block> = Vec::new();
    for (bi, &start) in starts.iter().enumerate() {
        let end = starts.get(bi + 1).copied().unwrap_or(ops.len());
        blocks.push(Block {
            start,
            end,
            succs: Vec::new(),
        });
    }
    let block_at: HashMap<usize, usize> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.start, i))
        .collect();

    for bi in 0..blocks.len() {
        let end = blocks[bi].end;
        if end == blocks[bi].start {
            continue;
        }
        let last = end - 1;
        match &ops[last] {
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target,
                ..
            } => {
                if let Some(&t) = label_at.get(&target.0)
                    && let Some(&sb) = block_at.get(&t)
                {
                    blocks[bi].succs.push(sb);
                }
            }
            IlOp::Jump { target, .. } => {
                if let Some(&t) = label_at.get(&target.0)
                    && let Some(&sb) = block_at.get(&t)
                {
                    blocks[bi].succs.push(sb);
                }
                if end < ops.len()
                    && let Some(&fb) = block_at.get(&end)
                {
                    blocks[bi].succs.push(fb);
                }
            }
            IlOp::Return { .. }
            | IlOp::Halt { .. }
            | IlOp::LoadReturnSlot { .. }
            | IlOp::ConstReturnImm { .. }
            | IlOp::BinReturn { .. } => {}
            _ => {
                if end < ops.len()
                    && let Some(&fb) = block_at.get(&end)
                {
                    blocks[bi].succs.push(fb);
                }
            }
        }
    }
    blocks
}

pub(crate) fn preds_of(blocks: &[Block]) -> Vec<Vec<usize>> {
    let mut preds = vec![Vec::new(); blocks.len()];
    for (i, b) in blocks.iter().enumerate() {
        for &s in &b.succs {
            preds[s].push(i);
        }
    }
    preds
}

/// Per-op slot use/def for liveness. `opaque` means residual forms whose slot
/// footprint is incomplete — coalescing that touches those ops is refused.
pub(crate) fn op_slot_use_def(op: &IlOp) -> (HashSet<u32>, HashSet<u32>, bool) {
    let mut uses = HashSet::new();
    let mut defs = HashSet::new();
    let mut opaque = false;
    match op {
        IlOp::Load { slot, .. } | IlOp::LoadReturnSlot { slot, .. } => {
            uses.insert(*slot);
        }
        IlOp::StorePop { slot, .. } => {
            defs.insert(*slot);
        }
        IlOp::BinSlotImm { slot, .. } => {
            uses.insert(*slot as u32);
        }
        IlOp::BinSlotSlot { a, b, .. } => {
            uses.insert(*a as u32);
            uses.insert(*b as u32);
        }
        IlOp::Byte { byte, .. } => {
            let insn = *byte.bytecode();
            match insn {
                Instruction::LOAD | Instruction::LoadReturnSlot => {
                    for k in 0..byte.load_store_count() {
                        uses.insert(byte.load_store_slot_at(k));
                    }
                }
                Instruction::STORE | Instruction::StorePop => {
                    for k in 0..byte.load_store_count() {
                        defs.insert(byte.load_store_slot_at(k));
                    }
                }
                Instruction::BinSlotImm | Instruction::BinSlotImmJmpf | Instruction::BinSlotImmJmpt => {
                    let (_, slot, _) = byte.bin_slot_imm_parts();
                    uses.insert(slot as u32);
                }
                Instruction::BinSlotImmStore => {
                    let (_, src, _) = byte.bin_slot_imm_store_parts();
                    uses.insert(src as u32);
                    opaque = true;
                }
                Instruction::BinSlotSlot | Instruction::BinSlotSlotJmpf | Instruction::BinSlotSlotJmpt => {
                    let (_, a, b) = byte.bin_slot_slot_parts();
                    uses.insert(a as u32);
                    uses.insert(b as u32);
                }
                Instruction::BinSlotSlotStore => {
                    let (_, a, b, dest) = byte.bin_slot_slot_store_parts();
                    uses.insert(a as u32);
                    uses.insert(b as u32);
                    defs.insert(dest as u32);
                }
                Instruction::BinSlotSlotConstJmpf | Instruction::BinSlotSlotConstJmpt => {
                    let o = byte.operand_u32();
                    uses.insert(((o >> 16) & 0xff) as u32);
                    opaque = true;
                }
                Instruction::FloatChainStore => {
                    let dest = byte.operand_u32() >> 16;
                    defs.insert(dest);
                    opaque = true;
                }
                _ => opaque = true,
            }
        }
        IlOp::Label(_)
        | IlOp::Jump { .. }
        | IlOp::Const { .. }
        | IlOp::ConstPool { .. }
        | IlOp::String { .. }
        | IlOp::Dup { .. }
        | IlOp::Pop { .. }
        | IlOp::Bin { .. }
        | IlOp::Return { .. }
        | IlOp::Halt { .. }
        | IlOp::ConstReturnImm { .. } => {}
        _ => opaque = true,
    }
    (uses, defs, opaque)
}

pub(crate) struct SlotLiveness {
    /// Slots live immediately before each op.
    pub(crate) live_before: Vec<HashSet<u32>>,
    /// Slots live on exit from each block.
    pub(crate) live_out: Vec<HashSet<u32>>,
    /// Ops whose slot footprint is incompletely known.
    pub(crate) opaque: Vec<bool>,
}

pub(crate) fn analyze_slot_liveness(ops: &[IlOp], blocks: &[Block]) -> SlotLiveness {
    let n = ops.len();
    let mut use_b: Vec<HashSet<u32>> = vec![HashSet::new(); blocks.len()];
    let mut def_b: Vec<HashSet<u32>> = vec![HashSet::new(); blocks.len()];
    let mut opaque = vec![false; n];

    for (bi, block) in blocks.iter().enumerate() {
        let mut defined = HashSet::new();
        for i in block.start..block.end {
            let (uses, defs, is_opaque) = op_slot_use_def(&ops[i]);
            opaque[i] = is_opaque;
            for u in &uses {
                if !defined.contains(u) {
                    use_b[bi].insert(*u);
                }
            }
            for d in &defs {
                defined.insert(*d);
                def_b[bi].insert(*d);
            }
        }
    }

    let mut live_in: Vec<HashSet<u32>> = vec![HashSet::new(); blocks.len()];
    let mut live_out: Vec<HashSet<u32>> = vec![HashSet::new(); blocks.len()];
    let mut changed = true;
    while changed {
        changed = false;
        for bi in (0..blocks.len()).rev() {
            let mut out = HashSet::new();
            for &s in &blocks[bi].succs {
                out.extend(live_in[s].iter().copied());
            }
            if out != live_out[bi] {
                live_out[bi] = out;
                changed = true;
            }
            let mut inn = use_b[bi].clone();
            for s in &live_out[bi] {
                if !def_b[bi].contains(s) {
                    inn.insert(*s);
                }
            }
            if inn != live_in[bi] {
                live_in[bi] = inn;
                changed = true;
            }
        }
    }

    let mut live_before = vec![HashSet::new(); n];
    for (bi, block) in blocks.iter().enumerate() {
        let mut live = live_out[bi].clone();
        for i in (block.start..block.end).rev() {
            let (uses, defs, _) = op_slot_use_def(&ops[i]);
            for d in &defs {
                live.remove(d);
            }
            live.extend(uses.iter().copied());
            live_before[i] = live.clone();
        }
    }

    SlotLiveness {
        live_before,
        live_out,
        opaque,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::op::IlOp;

    fn loc() -> common::DebugLoc {
        common::DebugLoc::unknown()
    }

    #[test]
    fn finds_unconditional_back_edge() {
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Const {
                imm: 1,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
        ];
        let loops = find_natural_loops(&ops);
        assert_eq!(loops.len(), 1);
        assert_eq!(loops[0].header, 0);
        assert_eq!(loops[0].latch, 2);
    }

    #[test]
    fn empty_ops_have_no_blocks() {
        assert!(build_blocks(&[]).is_empty());
    }
}
