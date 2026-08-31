//! CFG-local value numbering for stack IL.
//!
//! Builds a simple block CFG from labels and jumps inside one function body,
//! then CSE's identical pure stack producers (`Const` / `Load` / `Bin` /
//! `BinSlot*` / `Index` / `LoadField`) within a block. At joins, sinks a
//! redundant producer (or length-2 pure tail) when every predecessor ends with
//! the same ops and SP-in agrees.
//!
//! Limitations: no SSA rename of slots; effectful ops (`StorePop`, calls,
//! HostInvoke, SetField, …) are barriers; does not replace Ord-sensitive convoy
//! refuse rules — GVN feeds cleaner identical tails into those passes.
//! COI-82: this intra-block + join-sink ceiling is the contract; no CFG copy-prop.

use std::collections::{HashMap, HashSet};

use common::Instruction;

use super::op::{IlJumpKind, IlOp, Label};
use super::sp;

/// Pure stack producer suitable for local numbering / join CSE.
fn is_pure_producer(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Const { .. }
            | IlOp::ConstPool { .. }
            | IlOp::String { .. }
            | IlOp::Load { .. }
            | IlOp::Bin { .. }
            | IlOp::BinSlotImm { .. }
            | IlOp::BinSlotSlot { .. }
            | IlOp::Index { .. }
            | IlOp::IndexUnchecked { .. }
            | IlOp::Dup { .. }
            | IlOp::LoadField { .. }
    ) || matches!(
        op.as_encode_byte(),
        Some(b) if matches!(
            *b.bytecode(),
            Instruction::CONST
                | Instruction::STRING
                | Instruction::LOAD
                | Instruction::ADD
                | Instruction::SUB
                | Instruction::MUL
                | Instruction::DIV
                | Instruction::MOD
                | Instruction::BinSlotImm
                | Instruction::BinSlotSlot
                | Instruction::Index
                | Instruction::DUPLICATE
                | Instruction::LoadField
        )
    )
}

fn is_mem_barrier(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::StorePop { .. }
            | IlOp::SetField { .. }
            | IlOp::HostInvoke { .. }
            | IlOp::Print { .. }
            | IlOp::Entry { .. }
            | IlOp::MakeTuple { .. }
            | IlOp::MakeArray { .. }
            | IlOp::MakeEnum { .. }
            | IlOp::BoxValue { .. }
            | IlOp::GetField { .. }
    ) || matches!(
        op.as_encode_byte(),
        Some(b) if matches!(
            *b.bytecode(),
            Instruction::STORE
                | Instruction::StorePop
                | Instruction::SetField
                | Instruction::HostInvoke
                | Instruction::PRINT
                | Instruction::CALL
                | Instruction::TailCall
                | Instruction::MakeCoro
                | Instruction::GetField
                | Instruction::MakeTuple
                | Instruction::MakeArray
                | Instruction::MakeEnum
                | Instruction::BoxValue
                | Instruction::FORMAT
                | Instruction::FfiInvoke
        )
    )
}

fn producer_key(op: &IlOp) -> Option<u64> {
    let b = op.as_encode_byte()?;
    if !is_pure_producer(op) {
        return None;
    }
    // Pack opcode + operand for identity.
    Some(((*b.bytecode() as u64) << 32) | (b.operand_u32() as u64))
}

#[derive(Clone, Debug)]
struct Block {
    start: usize,
    end: usize, // exclusive
    succs: Vec<usize>,
}

fn build_blocks(ops: &[IlOp]) -> Vec<Block> {
    if ops.is_empty() {
        return Vec::new();
    }
    let mut leaders: HashSet<usize> = HashSet::new();
    leaders.insert(0);
    let mut label_at: HashMap<u32, usize> = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) = op {
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
        let start = blocks[bi].start;
        let end = blocks[bi].end;
        if end == start {
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
        let _ = start;
    }
    blocks
}

/// Block ranges and predecessor lists for SSA-GVN (COI-121).
pub(crate) fn gvn_cfg(ops: &[IlOp]) -> (Vec<(usize, usize)>, Vec<Vec<usize>>) {
    let blocks = build_blocks(ops);
    let n = blocks.len();
    let mut preds = vec![Vec::new(); n];
    for (i, b) in blocks.iter().enumerate() {
        for &s in &b.succs {
            if s < n {
                preds[s].push(i);
            }
        }
    }
    let ranges = blocks.into_iter().map(|b| (b.start, b.end)).collect();
    (ranges, preds)
}

fn preds_of(blocks: &[Block]) -> Vec<Vec<usize>> {
    let mut preds = vec![Vec::new(); blocks.len()];
    for (i, b) in blocks.iter().enumerate() {
        for &s in &b.succs {
            preds[s].push(i);
        }
    }
    preds
}

/// Local CSE within each block: identical Const/Load → Dup; then LoadField CSE.
fn gvn_within_blocks(ops: &mut Vec<IlOp>, blocks: &[Block]) {
    for b in blocks {
        let mut last_key: Option<u64> = None;
        let mut last_idx: Option<usize> = None;
        for i in b.start..b.end {
            if matches!(ops[i], IlOp::Label(_) | IlOp::JoinLabel(_)) || is_mem_barrier(&ops[i]) {
                last_key = None;
                last_idx = None;
                continue;
            }
            if !is_pure_producer(&ops[i]) {
                last_key = None;
                last_idx = None;
                continue;
            }
            let key = producer_key(&ops[i]);
            if let (Some(k), Some(pk), Some(pi)) = (key, last_key, last_idx)
                && k == pk
                && matches!(
                    &ops[pi],
                    // STRING stays: Dup-CSE of identical table hits breaks
                    // nested `"%s%s"` FORMAT concat (HTTP header lines).
                    IlOp::Const { .. } | IlOp::ConstPool { .. } | IlOp::Load { .. }
                )
                && matches!(
                    &ops[i],
                    IlOp::Const { .. } | IlOp::ConstPool { .. } | IlOp::Load { .. }
                )
            {
                ops[i] = IlOp::Dup { loc: ops[i].loc() };
                // Keep the original producer key so Const;Const;Const → Const;Dup;Dup.
                continue;
            }
            last_key = key;
            last_idx = Some(i);
        }
    }
    load_field_cse(ops, blocks);
    // load_field_cse may shrink `ops`; rebuild block bounds before GetField CSE.
    let blocks = build_blocks(ops);
    get_field_cse(ops, &blocks);
}

/// `Load obj; Load key; GetField` twice → drop second loads, replace second
/// GetField with `Dup` when only labels intervene (same field still TOS).
fn get_field_cse(ops: &mut Vec<IlOp>, blocks: &[Block]) {
    let mut remove: HashSet<usize> = HashSet::new();
    for b in blocks {
        let mut last: Option<(u32, u32, usize)> = None; // obj, key, getfield_idx
        let mut i = b.start;
        while i < b.end {
            if matches!(ops[i], IlOp::Label(_) | IlOp::JoinLabel(_)) {
                i += 1;
                continue;
            }
            // SetField / stores / calls invalidate; GetField itself is the CSE site.
            if is_get_field_barrier(&ops[i]) {
                last = None;
                i += 1;
                continue;
            }
            if i + 2 < b.end
                && let (
                    IlOp::Load { slot: obj, .. },
                    IlOp::Load { slot: key, .. },
                    IlOp::GetField { loc },
                ) = (&ops[i], &ops[i + 1], &ops[i + 2])
            {
                let obj = *obj;
                let key = *key;
                let loc = *loc;
                if let Some((po, pk, fi)) = last
                    && po == obj
                    && pk == key
                {
                    let mut only_labels = true;
                    for j in fi + 1..i {
                        if !matches!(ops[j], IlOp::Label(_) | IlOp::JoinLabel(_)) {
                            only_labels = false;
                            break;
                        }
                    }
                    if only_labels {
                        remove.insert(i);
                        remove.insert(i + 1);
                        ops[i + 2] = IlOp::Dup { loc };
                        last = Some((obj, key, i + 2));
                        i += 3;
                        continue;
                    }
                }
                last = Some((obj, key, i + 2));
                i += 3;
                continue;
            }
            if !matches!(
                &ops[i],
                IlOp::Const { .. }
                    | IlOp::ConstPool { .. }
                    | IlOp::BinSlotImm { .. }
                    | IlOp::BinSlotSlot { .. }
                    | IlOp::Dup { .. }
            ) {
                last = None;
            }
            i += 1;
        }
    }
    if remove.is_empty() {
        return;
    }
    let mut out = Vec::with_capacity(ops.len());
    for (i, op) in ops.iter().enumerate() {
        if !remove.contains(&i) {
            out.push(op.clone());
        }
    }
    *ops = out;
}

fn is_get_field_barrier(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::StorePop { .. }
            | IlOp::SetField { .. }
            | IlOp::HostInvoke { .. }
            | IlOp::Print { .. }
            | IlOp::Entry { .. }
            | IlOp::MakeTuple { .. }
            | IlOp::MakeArray { .. }
            | IlOp::MakeEnum { .. }
            | IlOp::BoxValue { .. }
    ) || matches!(
        op.as_encode_byte(),
        Some(b) if matches!(
            *b.bytecode(),
            Instruction::STORE
                | Instruction::StorePop
                | Instruction::SetField
                | Instruction::HostInvoke
                | Instruction::PRINT
                | Instruction::CALL
                | Instruction::TailCall
                | Instruction::MakeCoro
                | Instruction::MakeTuple
                | Instruction::MakeArray
                | Instruction::MakeEnum
                | Instruction::BoxValue
                | Instruction::FORMAT
                | Instruction::FfiInvoke
                | Instruction::StoreIndex
                | Instruction::StoreIndexUnchecked
                | Instruction::StoreIndexPin
                | Instruction::StoreIndexPinUnchecked
                | Instruction::ArrayPush
        )
    )
}

/// `Load s; LoadField i; Load s; LoadField i` → drop second Load, replace second
/// LoadField with `Dup` when only labels intervene (field still TOS).
fn load_field_cse(ops: &mut Vec<IlOp>, blocks: &[Block]) {
    let mut remove: HashSet<usize> = HashSet::new();
    for b in blocks {
        let mut last: Option<(u32, u32, usize)> = None; // slot, index, field_idx
        let mut i = b.start;
        while i < b.end {
            if matches!(ops[i], IlOp::Label(_) | IlOp::JoinLabel(_)) {
                i += 1;
                continue;
            }
            if is_mem_barrier(&ops[i]) {
                last = None;
                i += 1;
                continue;
            }
            if i + 1 < b.end
                && let (IlOp::Load { slot, .. }, IlOp::LoadField { index, loc }) =
                    (&ops[i], &ops[i + 1])
            {
                let slot = *slot;
                let index = *index;
                let loc = *loc;
                if let Some((ps, pi, fi)) = last
                    && ps == slot
                    && pi == index
                {
                    let mut only_labels = true;
                    for j in fi + 1..i {
                        if !matches!(ops[j], IlOp::Label(_) | IlOp::JoinLabel(_)) {
                            only_labels = false;
                            break;
                        }
                    }
                    if only_labels {
                        remove.insert(i);
                        ops[i + 1] = IlOp::Dup { loc };
                        last = Some((slot, index, i + 1));
                        i += 2;
                        continue;
                    }
                }
                last = Some((slot, index, i + 1));
                i += 2;
                continue;
            }
            if !matches!(
                &ops[i],
                IlOp::Const { .. }
                    | IlOp::ConstPool { .. }
                    | IlOp::BinSlotImm { .. }
                    | IlOp::BinSlotSlot { .. }
            ) {
                last = None;
            }
            i += 1;
        }
    }
    if remove.is_empty() {
        return;
    }
    let mut out = Vec::with_capacity(ops.len());
    for (i, op) in ops.iter().enumerate() {
        if !remove.contains(&i) {
            out.push(op.clone());
        }
    }
    *ops = out;
}

/// At a join block, if every pred ends with the same pure Const/Load (or the
/// same length-2 pure tail) and the join starts with that copy, drop the join
/// copy. Requires Known agreeing SP at join.
fn gvn_at_joins(ops: &mut Vec<IlOp>, blocks: &[Block]) {
    if blocks.is_empty() {
        return;
    }
    let info = sp::analyze(ops);
    let preds = preds_of(blocks);
    let mut remove: HashSet<usize> = HashSet::new();

    for (bi, b) in blocks.iter().enumerate() {
        if preds[bi].len() < 2 {
            continue;
        }
        if !info.sp_before(b.start).is_known() {
            continue;
        }

        // Prefer length-2 pure tail CSE, then single Const/Load.
        if let Some(tail) = join_pure_tail(ops, b.start, b.end, 2) {
            let keys: Vec<u64> = tail.iter().filter_map(|&i| producer_key(&ops[i])).collect();
            if keys.len() == 2
                && preds[bi]
                    .iter()
                    .all(|&p| pred_tail_keys(ops, &blocks[p], 2).as_ref() == Some(&keys))
            {
                for &i in &tail {
                    remove.insert(i);
                }
                continue;
            }
        }

        let mut join_prod = None;
        for i in b.start..b.end {
            if matches!(ops[i], IlOp::Label(_) | IlOp::JoinLabel(_)) {
                continue;
            }
            if is_pure_producer(&ops[i])
                && matches!(
                    &ops[i],
                    IlOp::Const { .. }
                        | IlOp::ConstPool { .. }
                        | IlOp::String { .. }
                        | IlOp::Load { .. }
                )
            {
                join_prod = Some(i);
            }
            break;
        }
        let Some(ji) = join_prod else {
            continue;
        };
        let Some(jk) = producer_key(&ops[ji]) else {
            continue;
        };

        let mut ok = true;
        for &p in &preds[bi] {
            let Some(pi) = last_emitting_non_jump(ops, &blocks[p]) else {
                ok = false;
                break;
            };
            if producer_key(&ops[pi]) != Some(jk) {
                ok = false;
                break;
            }
            if !matches!(
                &ops[pi],
                IlOp::Const { .. }
                    | IlOp::ConstPool { .. }
                    | IlOp::String { .. }
                    | IlOp::Load { .. }
            ) {
                ok = false;
                break;
            }
        }
        if ok {
            remove.insert(ji);
        }
    }

    if remove.is_empty() {
        return;
    }
    let mut out = Vec::with_capacity(ops.len());
    for (i, op) in ops.iter().enumerate() {
        if !remove.contains(&i) {
            out.push(op.clone());
        }
    }
    *ops = out;
}

fn last_emitting_non_jump(ops: &[IlOp], b: &Block) -> Option<usize> {
    if b.end == b.start {
        return None;
    }
    for i in (b.start..b.end).rev() {
        if matches!(ops[i], IlOp::Label(_) | IlOp::JoinLabel(_)) {
            continue;
        }
        if matches!(ops[i], IlOp::Jump { .. }) {
            continue;
        }
        if is_return_like(&ops[i]) {
            continue;
        }
        return Some(i);
    }
    None
}

/// First `len` consecutive pure producers in a join block (skipping labels).
fn join_pure_tail(ops: &[IlOp], start: usize, end: usize, len: usize) -> Option<Vec<usize>> {
    let mut idxs = Vec::with_capacity(len);
    for i in start..end {
        if matches!(ops[i], IlOp::Label(_) | IlOp::JoinLabel(_)) {
            continue;
        }
        if !is_pure_producer(&ops[i]) {
            break;
        }
        // Length-2 join CSE: Const/Load/Bin/BinSlot/Index/LoadField only.
        if !matches!(
            &ops[i],
            IlOp::Const { .. }
                | IlOp::ConstPool { .. }
                | IlOp::Load { .. }
                | IlOp::Bin { .. }
                | IlOp::BinSlotImm { .. }
                | IlOp::BinSlotSlot { .. }
                | IlOp::Index { .. }
            | IlOp::IndexUnchecked { .. }
                | IlOp::LoadField { .. }
                | IlOp::Dup { .. }
        ) {
            break;
        }
        idxs.push(i);
        if idxs.len() == len {
            return Some(idxs);
        }
    }
    None
}

fn pred_tail_keys(ops: &[IlOp], b: &Block, len: usize) -> Option<Vec<u64>> {
    let mut emitting = Vec::new();
    for i in b.start..b.end {
        if matches!(ops[i], IlOp::Label(_) | IlOp::JoinLabel(_) | IlOp::Jump { .. }) || is_return_like(&ops[i]) {
            continue;
        }
        emitting.push(i);
    }
    if emitting.len() < len {
        return None;
    }
    let tail = &emitting[emitting.len() - len..];
    let mut keys = Vec::with_capacity(len);
    for &i in tail {
        if !is_pure_producer(&ops[i]) {
            return None;
        }
        keys.push(producer_key(&ops[i])?);
    }
    Some(keys)
}

fn is_return_like(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Return { .. }
            | IlOp::Halt { .. }
            | IlOp::LoadReturnSlot { .. }
            | IlOp::ConstReturnImm { .. }
            | IlOp::BinReturn { .. }
    )
}

/// Run CFG-local GVN on a single function body in place.
#[allow(dead_code)]
pub fn cfg_gvn(ops: &mut Vec<IlOp>) {
    cfg_gvn_with(ops, true);
}

/// Like [`cfg_gvn`], with SSA global numbering optional (COI-121).
pub fn cfg_gvn_with(ops: &mut Vec<IlOp>, ssa: bool) {
    if ops.len() < 2 {
        return;
    }
    let blocks = build_blocks(ops);
    gvn_within_blocks(ops, &blocks);
    let blocks = build_blocks(ops);
    gvn_at_joins(ops, &blocks);
    if ssa {
        super::gvn_ssa::ssa_gvn(ops);
    }
}

pub use super::gvn_ssa::{build_ssa, eliminate_redundant, number_values, ssa_gvn};

#[cfg(test)]
#[path = "gvn.tests.rs"]
mod ssa_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn within_block_dup_replaces_second_identical_const() {
        let mut ops = vec![
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::Return { loc: loc() },
        ];
        cfg_gvn(&mut ops);
        assert!(matches!(ops[0], IlOp::Const { imm: 3, .. }));
        assert!(matches!(ops[1], IlOp::Dup { .. }));
    }

    #[test]
    fn within_block_const_run_compresses_to_dup_chain() {
        let mut ops = vec![
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::Return { loc: loc() },
        ];
        cfg_gvn(&mut ops);
        assert!(matches!(ops[0], IlOp::Const { imm: 3, .. }));
        assert!(matches!(ops[1], IlOp::Dup { .. }));
        assert!(matches!(ops[2], IlOp::Dup { .. }));
    }

    /// Identical loads CSE to `Load; Dup`; fuse-select treats Dup as the second operand.
    #[test]
    fn within_block_dup_replaces_second_identical_load() {
        let mut ops = vec![
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::Return { loc: loc() },
        ];
        let blocks = build_blocks(&ops);
        gvn_within_blocks(&mut ops, &blocks);
        assert!(matches!(ops[0], IlOp::Load { slot: 2, .. }));
        assert!(matches!(ops[1], IlOp::Dup { .. }));
    }

    #[test]
    fn within_block_store_pop_is_barrier() {
        // Effectful StorePop must reset numbering — second Const stays Const.
        let mut ops = vec![
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::StorePop {
                slot: 0,
                loc: loc(),
            },
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::Return { loc: loc() },
        ];
        cfg_gvn(&mut ops);
        assert!(matches!(ops[2], IlOp::Const { imm: 3, .. }));
        assert!(!ops.iter().any(|op| matches!(op, IlOp::Dup { .. })));
    }

    #[test]
    fn within_block_host_invoke_is_barrier() {
        let mut ops = vec![
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::HostInvoke {
                arity: 0,
                loc: loc(),
            },
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::Return { loc: loc() },
        ];
        cfg_gvn(&mut ops);
        assert!(matches!(ops[2], IlOp::Const { imm: 3, .. }));
        assert!(!ops.iter().any(|op| matches!(op, IlOp::Dup { .. })));
    }

    #[test]
    fn within_block_string_not_dup_cse() {
        let mut ops = vec![
            IlOp::String { idx: 4, loc: loc() },
            IlOp::String { idx: 4, loc: loc() },
            IlOp::Return { loc: loc() },
        ];
        cfg_gvn(&mut ops);
        assert!(matches!(ops[0], IlOp::String { idx: 4, .. }));
        assert!(
            matches!(ops[1], IlOp::String { idx: 4, .. }),
            "STRING must not Dup-CSE (FORMAT concat / http showcase)"
        );
        assert!(!ops.iter().any(|op| matches!(op, IlOp::Dup { .. })));
    }

    /// Nested `a + b + c` emits consecutive `"%s%s"` STRING ops (outer then
    /// inner). Dup-CSE would rewrite the second to DUP and poison FORMAT.
    #[test]
    fn within_block_nested_format_concat_keeps_pct_s_strings() {
        let mut ops = vec![
            IlOp::String { idx: 0, loc: loc() }, // outer "%s%s"
            IlOp::String { idx: 0, loc: loc() }, // inner "%s%s"
            IlOp::String { idx: 1, loc: loc() }, // a
            IlOp::String { idx: 2, loc: loc() }, // b
            IlOp::byte(common::Byte::new(Instruction::FORMAT).with_operand_u32(2)),
            IlOp::String { idx: 3, loc: loc() }, // c
            IlOp::byte(common::Byte::new(Instruction::FORMAT).with_operand_u32(2)),
            IlOp::Return { loc: loc() },
        ];
        cfg_gvn(&mut ops);
        let pct_s = ops
            .iter()
            .filter(|op| matches!(op, IlOp::String { idx: 0, .. }))
            .count();
        assert_eq!(
            pct_s, 2,
            "both \"%s%s\" STRINGs must remain for nested FORMAT concat"
        );
        assert!(
            !ops.iter().any(|op| matches!(op, IlOp::Dup { .. })),
            "nested FORMAT concat must not Dup-CSE STRING"
        );
    }

    /// Triple nest `((a+b)+c)+d` — Dup chain would rewrite all but the first
    /// `"%s%s"` if STRING were still in the within-block allowlist.
    #[test]
    fn within_block_triple_nested_format_keeps_all_pct_s_strings() {
        let mut ops = vec![
            IlOp::String { idx: 0, loc: loc() },
            IlOp::String { idx: 0, loc: loc() },
            IlOp::String { idx: 0, loc: loc() },
            IlOp::String { idx: 1, loc: loc() },
            IlOp::String { idx: 2, loc: loc() },
            IlOp::byte(common::Byte::new(Instruction::FORMAT).with_operand_u32(2)),
            IlOp::String { idx: 3, loc: loc() },
            IlOp::byte(common::Byte::new(Instruction::FORMAT).with_operand_u32(2)),
            IlOp::String { idx: 4, loc: loc() },
            IlOp::byte(common::Byte::new(Instruction::FORMAT).with_operand_u32(2)),
            IlOp::Return { loc: loc() },
        ];
        cfg_gvn(&mut ops);
        let pct_s = ops
            .iter()
            .filter(|op| matches!(op, IlOp::String { idx: 0, .. }))
            .count();
        assert_eq!(pct_s, 3, "all three \"%s%s\" STRINGs must remain");
        assert!(!ops.iter().any(|op| matches!(op, IlOp::Dup { .. })));
    }

    #[test]
    fn within_block_string_refuses_mismatched_idx() {
        let mut ops = vec![
            IlOp::String { idx: 4, loc: loc() },
            IlOp::String { idx: 5, loc: loc() },
            IlOp::Return { loc: loc() },
        ];
        cfg_gvn(&mut ops);
        assert!(matches!(ops[0], IlOp::String { idx: 4, .. }));
        assert!(matches!(ops[1], IlOp::String { idx: 5, .. }));
        assert!(!ops.iter().any(|op| matches!(op, IlOp::Dup { .. })));
    }

    #[test]
    fn join_cse_drops_redundant_string_on_jmpf_diamond() {
        // JMPF diamond with agreeing Known SP: join STRING is redundant.
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::String { idx: 7, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::String { idx: 7, loc: loc() },
            IlOp::Label(Label(2)),
            IlOp::String { idx: 7, loc: loc() },
            IlOp::Return { loc: loc() },
        ];
        let join = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(2))))
            .unwrap();
        let info = sp::analyze(&ops);
        assert!(
            info.sp_before(join).is_known() || info.sp_before(join + 1).is_known(),
            "precondition: join region has Known SP"
        );
        cfg_gvn(&mut ops);
        let strings = ops
            .iter()
            .filter(|op| matches!(op, IlOp::String { idx: 7, .. }))
            .count();
        assert_eq!(strings, 2, "pred STRINGs kept; join copy dropped");
        assert!(ops.iter().any(|op| matches!(op, IlOp::Return { .. })));
    }

    #[test]
    fn join_cse_keeps_disagreeing_string() {
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::String { idx: 1, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::String { idx: 2, loc: loc() },
            IlOp::Label(Label(2)),
            IlOp::String { idx: 1, loc: loc() },
            IlOp::Return { loc: loc() },
        ];
        let before_len = ops.len();
        cfg_gvn(&mut ops);
        assert_eq!(
            ops.len(),
            before_len,
            "disagreeing STRING preds must not drop join"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::String { idx: 1, .. }))
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::String { idx: 2, .. }))
        );
    }

    #[test]
    fn within_block_const_pool_dup_cse() {
        let mut ops = vec![
            IlOp::ConstPool { idx: 2, loc: loc() },
            IlOp::ConstPool { idx: 2, loc: loc() },
            IlOp::Return { loc: loc() },
        ];
        cfg_gvn(&mut ops);
        assert!(matches!(ops[0], IlOp::ConstPool { idx: 2, .. }));
        assert!(matches!(ops[1], IlOp::Dup { .. }));
    }

    #[test]
    fn join_cse_drops_redundant_const_on_jmpf_diamond() {
        // JMPF diamond with agreeing Known SP: join CONST is redundant.
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Label(Label(2)),
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Return { loc: loc() },
        ];
        cfg_gvn(&mut ops);
        let consts = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Const { imm: 1, .. }))
            .count();
        assert_eq!(consts, 2, "pred consts kept; join copy dropped");
        assert!(ops.iter().any(|op| matches!(op, IlOp::Return { .. })));
    }

    #[test]
    fn join_cse_drops_redundant_load_on_jmpf_diamond() {
        // Same Known-SP diamond as const join CSE, with Load producers.
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Label(Label(2)),
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Return { loc: loc() },
        ];
        let info = sp::analyze(&ops);
        assert!(
            info.sp_before(6).is_known() || info.sp_before(7).is_known(),
            "precondition: join region has Known SP"
        );
        cfg_gvn(&mut ops);
        let loads = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { slot: 3, .. }))
            .count();
        assert_eq!(loads, 2, "pred loads kept; join Load dropped when SP Known");
        assert!(ops.iter().any(|op| matches!(op, IlOp::Return { .. })));
    }

    #[test]
    fn join_cse_keeps_disagreeing_const() {
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Label(Label(2)),
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Return { loc: loc() },
        ];
        let before_len = ops.len();
        cfg_gvn(&mut ops);
        assert_eq!(
            ops.len(),
            before_len,
            "disagreeing preds must not drop join"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::Const { imm: 1, .. }))
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::Const { imm: 2, .. }))
        );
    }

    #[test]
    fn join_cse_keeps_load_when_join_sp_unknown() {
        // FfiInvoke fail-closes SP; Known-SP Load join CSE must not fire.
        let mut ops = vec![
            IlOp::byte(common::Byte::new(common::Instruction::FfiInvoke)),
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Label(Label(2)),
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Return { loc: loc() },
        ];
        let join = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(2))))
            .unwrap();
        let info = sp::analyze(&ops);
        assert!(
            !info.sp_before(join).is_known(),
            "precondition: join SP must be Unknown"
        );
        cfg_gvn(&mut ops);
        let loads = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { slot: 3, .. }))
            .count();
        assert_eq!(loads, 3, "Unknown SP must keep join Load");
    }

    #[test]
    fn load_field_cse_replaces_redundant_pair_with_dup() {
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::LoadField {
                index: 1,
                loc: loc(),
            },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::LoadField {
                index: 1,
                loc: loc(),
            },
            IlOp::Return { loc: loc() },
        ];
        cfg_gvn(&mut ops);
        assert!(matches!(ops[0], IlOp::Load { slot: 0, .. }));
        assert!(matches!(ops[1], IlOp::LoadField { index: 1, .. }));
        assert!(matches!(ops[2], IlOp::Dup { .. }));
        assert_eq!(ops.len(), 4);
    }

    #[test]
    fn load_field_cse_refuses_across_set_field() {
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::LoadField {
                index: 1,
                loc: loc(),
            },
            IlOp::SetField { loc: loc() },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::LoadField {
                index: 1,
                loc: loc(),
            },
            IlOp::Return { loc: loc() },
        ];
        let before = ops.len();
        cfg_gvn(&mut ops);
        assert_eq!(ops.len(), before);
        assert_eq!(
            ops.iter()
                .filter(|op| matches!(op, IlOp::LoadField { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn get_field_cse_dups_repeated_obj_key_load() {
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::GetField { loc: loc() },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::GetField { loc: loc() },
            IlOp::Return { loc: loc() },
        ];
        cfg_gvn(&mut ops);
        assert_eq!(
            ops.iter()
                .filter(|op| matches!(op, IlOp::GetField { .. }))
                .count(),
            1
        );
        assert!(matches!(ops[2], IlOp::GetField { .. }));
        assert!(matches!(ops[3], IlOp::Dup { .. }));
    }

    #[test]
    fn get_field_cse_refuses_when_set_field_intervening() {
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::GetField { loc: loc() },
            IlOp::Pop { loc: loc() },
            IlOp::Const { imm: 9, loc: loc() },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::SetField { loc: loc() },
            IlOp::Pop { loc: loc() },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::GetField { loc: loc() },
            IlOp::Return { loc: loc() },
        ];
        cfg_gvn(&mut ops);
        assert_eq!(
            ops.iter()
                .filter(|op| matches!(op, IlOp::GetField { .. }))
                .count(),
            2,
            "SetField must invalidate GetField CSE"
        );
    }

    #[test]
    fn join_cse_drops_length_two_pure_tail() {
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Label(Label(2)),
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Return { loc: loc() },
        ];
        cfg_gvn(&mut ops);
        let join_loads = ops
            .iter()
            .skip_while(|op| !matches!(op, IlOp::Label(Label(2))))
            .filter(|op| matches!(op, IlOp::Load { slot: 0, .. }))
            .count();
        assert_eq!(join_loads, 0, "length-2 join tail should be sunk");
    }

    #[test]
    fn expands_dup_after_load_so_binop_can_fuse() {
        // `x * x`: GVN leaves `Load; Dup; MUL`; fuse-select treats Dup as the
        // second operand of BinSlotSlot.
        let mut ops = vec![
            IlOp::Load { slot: 7, loc: loc() },
            IlOp::Load { slot: 7, loc: loc() },
            IlOp::Bin {
                op: Instruction::MUL,
                loc: loc(),
            },
            IlOp::Return { loc: loc() },
        ];
        cfg_gvn(&mut ops);
        assert!(
            matches!(ops[0], IlOp::Load { slot: 7, .. }) && matches!(ops[1], IlOp::Dup { .. }),
            "GVN may Dup-CSE the second LOAD; fuse-select accepts Dup"
        );
    }

    #[test]
    fn expand_dup_after_load_leaves_const_dup_alone() {
        let mut ops = vec![
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::Return { loc: loc() },
        ];
        cfg_gvn(&mut ops);
        assert!(matches!(ops[1], IlOp::Dup { .. }));
    }

}
