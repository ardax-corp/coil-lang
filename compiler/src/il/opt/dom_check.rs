//! Dominating-check elimination (Opt5).
//!
//! Intra-function, fail-closed: a later bounds / niche / tag test drops or
//! becomes unchecked only when a dominating fact already proves it. Stores,
//! impure calls, host, yield, and unknown residual `Byte` kill facts.

use std::collections::{HashMap, HashSet};

use common::{Byte, Instruction};

use super::super::analysis::{build_blocks, preds_of, Block};
use super::super::op::{EntryKind, IlJumpKind, IlOp, Label};
use super::super::pure_call::{op_blocks_length_proof, PureCallCtx};

/// Rewrite dominated checks. Returns the number of sites changed.
#[cfg(test)]
pub(crate) fn dominate_checks(ops: &mut Vec<IlOp>) -> usize {
    dominate_checks_with(ops, None)
}

/// [`dominate_checks`] with an explicit purity table (pure `CALL` is not a
/// length / heap-fact barrier).
pub(crate) fn dominate_checks_with(ops: &mut Vec<IlOp>, purity: Option<&PureCallCtx>) -> usize {
    if ops.len() < 3 {
        return 0;
    }
    let blocks = build_blocks(ops);
    if blocks.is_empty() {
        return 0;
    }
    let preds = preds_of(&blocks);
    let order = rpo(&blocks);
    let mut out = vec![None; blocks.len()];
    let mut hints = vec![EdgeHint::None; blocks.len()];
    let mut before = vec![Facts::default(); ops.len()];

    for &bi in &order {
        let mut facts = inherit(ops, &blocks, &preds, &out, &hints, bi);
        let start = blocks[bi].start;
        let end = blocks[bi].end;
        for i in start..end {
            before[i] = facts.clone();
            apply_op(ops, i, &mut facts, purity);
        }
        if end > start {
            hints[bi] = edge_hint(ops, end - 1, &before[end - 1]);
        }
        out[bi] = Some(facts);
    }

    rewrite(ops, &before)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Idx {
    Slot(u32),
    Imm(i32),
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
struct Facts {
    in_bounds: HashSet<(u32, Idx)>,
    nonneg: HashSet<u32>,
    lt_len: HashSet<(u32, u32)>,
    exact: HashMap<u32, i32>,
    nonzero: HashSet<u32>,
    neq: HashSet<(u32, i32)>,
    len_of: HashMap<u32, u32>,
    enum_tag: HashMap<u32, u32>,
}

impl Facts {
    fn intersect(&self, other: &Self) -> Self {
        Self {
            in_bounds: self
                .in_bounds
                .intersection(&other.in_bounds)
                .copied()
                .collect(),
            nonneg: self.nonneg.intersection(&other.nonneg).copied().collect(),
            lt_len: self.lt_len.intersection(&other.lt_len).copied().collect(),
            exact: self
                .exact
                .iter()
                .filter_map(|(k, v)| other.exact.get(k).filter(|w| *w == v).map(|_| (*k, *v)))
                .collect(),
            nonzero: self.nonzero.intersection(&other.nonzero).copied().collect(),
            neq: self.neq.intersection(&other.neq).copied().collect(),
            len_of: self
                .len_of
                .iter()
                .filter_map(|(k, v)| other.len_of.get(k).filter(|w| *w == v).map(|_| (*k, *v)))
                .collect(),
            enum_tag: self
                .enum_tag
                .iter()
                .filter_map(|(k, v)| other.enum_tag.get(k).filter(|w| *w == v).map(|_| (*k, *v)))
                .collect(),
        }
    }

    fn kill_slot(&mut self, slot: u32) {
        self.in_bounds
            .retain(|(a, i)| *a != slot && *i != Idx::Slot(slot));
        self.nonneg.remove(&slot);
        self.lt_len.retain(|(i, a)| *i != slot && *a != slot);
        self.exact.remove(&slot);
        self.nonzero.remove(&slot);
        self.neq.retain(|(s, _)| *s != slot);
        self.len_of.remove(&slot);
        self.len_of.retain(|_, a| *a != slot);
        self.enum_tag.remove(&slot);
    }

    fn kill_heap(&mut self) {
        self.in_bounds.clear();
        self.lt_len.clear();
        self.len_of.clear();
    }

    fn kill_all(&mut self) {
        *self = Self::default();
    }

    fn set_exact(&mut self, slot: u32, imm: i32) {
        self.kill_slot(slot);
        self.exact.insert(slot, imm);
        if imm >= 0 {
            self.nonneg.insert(slot);
        }
        if imm != 0 {
            self.nonzero.insert(slot);
        }
        self.neq.retain(|(s, v)| *s != slot || *v != imm);
    }

    fn set_nonzero(&mut self, slot: u32) {
        self.exact.remove(&slot);
        self.nonzero.insert(slot);
        self.neq.insert((slot, 0));
    }

    fn copy_slot(&mut self, dest: u32, src: u32) {
        if dest == src {
            return;
        }
        let exact = self.exact.get(&src).copied();
        let nonneg = self.nonneg.contains(&src);
        let nonzero = self.nonzero.contains(&src);
        let neq: Vec<i32> = self
            .neq
            .iter()
            .filter(|(s, _)| *s == src)
            .map(|(_, v)| *v)
            .collect();
        let tag = self.enum_tag.get(&src).copied();
        let bounds: Vec<(u32, Idx)> = self
            .in_bounds
            .iter()
            .filter_map(|(a, i)| {
                if *a == src {
                    Some((dest, *i))
                } else if *i == Idx::Slot(src) {
                    Some((*a, Idx::Slot(dest)))
                } else {
                    None
                }
            })
            .collect();
        let lts: Vec<(u32, u32)> = self
            .lt_len
            .iter()
            .filter_map(|(i, a)| {
                if *i == src {
                    Some((dest, *a))
                } else if *a == src {
                    Some((*i, dest))
                } else {
                    None
                }
            })
            .collect();
        let lens: Vec<(u32, u32)> = self
            .len_of
            .iter()
            .filter_map(|(s, a)| {
                if *s == src {
                    Some((dest, *a))
                } else if *a == src {
                    Some((*s, dest))
                } else {
                    None
                }
            })
            .collect();
        self.kill_slot(dest);
        if let Some(v) = exact {
            self.set_exact(dest, v);
        } else {
            if nonneg {
                self.nonneg.insert(dest);
            }
            if nonzero {
                self.set_nonzero(dest);
            }
            for v in neq {
                self.neq.insert((dest, v));
            }
        }
        if let Some(t) = tag {
            self.enum_tag.insert(dest, t);
        }
        self.in_bounds.extend(bounds);
        self.lt_len.extend(lts);
        for (s, a) in lens {
            self.len_of.insert(s, a);
        }
    }

    fn prove_index(&mut self, arr: u32, idx: Idx) {
        if matches!(idx, Idx::Imm(k) if k < 0) {
            return;
        }
        self.in_bounds.insert((arr, idx));
        if let Idx::Slot(i) = idx {
            self.nonneg.insert(i);
            self.lt_len.insert((i, arr));
        }
    }

    fn has_index(&self, arr: u32, idx: Idx) -> bool {
        self.in_bounds.contains(&(arr, idx))
            || match idx {
                Idx::Slot(i) => self.nonneg.contains(&i) && self.lt_len.contains(&(i, arr)),
                Idx::Imm(_) => false,
            }
    }

    fn eq_result(&self, slot: u32, expected: i32) -> Option<i32> {
        if let Some(v) = self.exact.get(&slot) {
            return Some(i32::from(*v == expected));
        }
        if let Some(t) = self.enum_tag.get(&slot) {
            return Some(i32::from(*t as i32 == expected));
        }
        if self.neq.contains(&(slot, expected)) {
            return Some(0);
        }
        if expected == 0 && self.nonzero.contains(&slot) {
            return Some(0);
        }
        None
    }
}

#[derive(Clone, Debug)]
enum EdgeHint {
    None,
    Cond {
        fall: Facts,
        taken: Facts,
        target: Label,
    },
}

fn rpo(blocks: &[Block]) -> Vec<usize> {
    let mut seen = vec![false; blocks.len()];
    let mut post = Vec::new();
    fn dfs(blocks: &[Block], seen: &mut [bool], post: &mut Vec<usize>, i: usize) {
        if seen[i] {
            return;
        }
        seen[i] = true;
        for &s in &blocks[i].succs {
            dfs(blocks, seen, post, s);
        }
        post.push(i);
    }
    if !blocks.is_empty() {
        dfs(blocks, &mut seen, &mut post, 0);
        for i in 0..blocks.len() {
            dfs(blocks, &mut seen, &mut post, i);
        }
    }
    post.reverse();
    post
}

fn inherit(
    ops: &[IlOp],
    blocks: &[Block],
    preds: &[Vec<usize>],
    out: &[Option<Facts>],
    hints: &[EdgeHint],
    bi: usize,
) -> Facts {
    let ps = &preds[bi];
    if ps.is_empty() {
        return Facts::default();
    }
    if ps.iter().any(|&p| out[p].is_none()) {
        return Facts::default();
    }
    let mut acc: Option<Facts> = None;
    for &p in ps {
        let f = refine_edge(ops, blocks, hints, p, bi, out[p].clone().unwrap_or_default());
        acc = Some(match acc {
            None => f,
            Some(prev) => prev.intersect(&f),
        });
    }
    acc.unwrap_or_default()
}

fn edge_hint(ops: &[IlOp], last: usize, facts: &Facts) -> EdgeHint {
    let IlOp::Jump {
        kind,
        target,
        ..
    } = &ops[last]
    else {
        return EdgeHint::None;
    };
    match kind {
        IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue => {
            let jmpf = matches!(kind, IlJumpKind::JumpIfFalse);
            if let Some((slot, expected)) = cond_eq_slot(ops, last) {
                let mut yes = facts.clone();
                let mut no = facts.clone();
                yes.set_exact(slot, expected);
                no.neq.insert((slot, expected));
                if expected == 0 {
                    no.set_nonzero(slot);
                }
                // JMPF taken when cond == 0 (EQ false). Fallthrough = EQ true.
                let (fall, taken) = if jmpf { (yes, no) } else { (no, yes) };
                return EdgeHint::Cond {
                    fall,
                    taken,
                    target: *target,
                };
            }
            if let Some((idx, bound)) = cond_lt_slots(ops, last) {
                let mut yes = facts.clone();
                if let Some(&arr) = facts.len_of.get(&bound) {
                    yes.lt_len.insert((idx, arr));
                    if yes.nonneg.contains(&idx) {
                        yes.prove_index(arr, Idx::Slot(idx));
                    }
                }
                let (fall, taken) = if jmpf {
                    (yes, facts.clone())
                } else {
                    (facts.clone(), yes)
                };
                return EdgeHint::Cond {
                    fall,
                    taken,
                    target: *target,
                };
            }
            EdgeHint::None
        }
        IlJumpKind::JumpIfMatch { tag, .. } => {
            if let Some(slot) = match_scrutinee_slot(ops, last) {
                let mut yes = facts.clone();
                let mut no = facts.clone();
                yes.enum_tag.insert(slot, *tag);
                if *tag == 0 {
                    yes.set_exact(slot, 0);
                } else {
                    yes.set_nonzero(slot);
                }
                no.neq.insert((slot, *tag as i32));
                if *tag == 0 {
                    no.set_nonzero(slot);
                }
                return EdgeHint::Cond {
                    fall: no,
                    taken: yes,
                    target: *target,
                };
            }
            EdgeHint::None
        }
        _ => EdgeHint::None,
    }
}

fn cond_eq_slot(ops: &[IlOp], jmp: usize) -> Option<(u32, i32)> {
    // LOAD s; DUP; CONST e; EQ; JMP
    if jmp >= 4
        && matches!(ops[jmp - 1], IlOp::Bin { op: Instruction::EQ, .. })
        && let IlOp::Const { imm: expected, .. } = ops[jmp - 2]
        && matches!(ops[jmp - 3], IlOp::Dup { .. })
        && let IlOp::Load { slot, .. } = ops[jmp - 4]
    {
        return Some((slot, expected));
    }
    // LOAD s; CONST e; EQ; JMP
    if jmp >= 3
        && matches!(ops[jmp - 1], IlOp::Bin { op: Instruction::EQ, .. })
        && let IlOp::Const { imm: expected, .. } = ops[jmp - 2]
        && let IlOp::Load { slot, .. } = ops[jmp - 3]
    {
        return Some((slot, expected));
    }
    None
}

fn cond_lt_slots(ops: &[IlOp], jmp: usize) -> Option<(u32, u32)> {
    if jmp >= 3
        && matches!(ops[jmp - 1], IlOp::Bin { op: Instruction::LE, .. })
        && let IlOp::Load { slot: bound, .. } = ops[jmp - 2]
        && let IlOp::Load { slot: idx, .. } = ops[jmp - 3]
    {
        return Some((idx, bound));
    }
    None
}

fn match_scrutinee_slot(ops: &[IlOp], jmp: usize) -> Option<u32> {
    match ops.get(jmp.checked_sub(1)?)? {
        IlOp::Load { slot, .. } => Some(*slot),
        _ => None,
    }
}

fn apply_op(ops: &[IlOp], i: usize, facts: &mut Facts, purity: Option<&PureCallCtx>) {
    match &ops[i] {
        IlOp::StorePop { slot, .. } => apply_store(ops, i, *slot, facts),
        IlOp::Index { .. } | IlOp::IndexUnchecked { .. } => {
            if let Some((arr, idx)) = index_operands(ops, i) {
                facts.prove_index(arr, idx);
            }
        }
        IlOp::IndexPin { slot, .. } | IlOp::IndexPinUnchecked { slot, .. } => {
            if let Some(idx) = index_only(ops, i) {
                facts.prove_index(*slot, idx);
            }
        }
        IlOp::StoreIndexPin { slot, .. } | IlOp::StoreIndexPinUnchecked { slot, .. } => {
            if let Some(idx) = store_pin_idx(ops, i) {
                facts.prove_index(*slot, idx);
            }
        }
        IlOp::Entry {
            kind: EntryKind::Call,
            target,
            ..
        } => {
            if !purity.is_some_and(|c| c.call_is_pure(*target)) {
                facts.kill_heap();
            }
        }
        IlOp::Entry { .. }
        | IlOp::HostInvoke { .. }
        | IlOp::Print { .. }
        | IlOp::GetField { .. }
        | IlOp::SetField { .. }
        | IlOp::MakeArray { .. }
        | IlOp::MakeTuple { .. }
        | IlOp::MakeEnum { .. } => facts.kill_heap(),
        IlOp::Byte { byte, .. } => apply_byte(ops, i, byte, facts, purity),
        IlOp::Jump { .. }
        | IlOp::Label(_)
        | IlOp::JoinLabel(_)
        | IlOp::Load { .. }
        | IlOp::Const { .. }
        | IlOp::ConstPool { .. }
        | IlOp::String { .. }
        | IlOp::Dup { .. }
        | IlOp::Pop { .. }
        | IlOp::LogNot { .. }
        | IlOp::Bin { .. }
        | IlOp::BinSlotImm { .. }
        | IlOp::BinSlotSlot { .. }
        | IlOp::ArrayPin { .. }
        | IlOp::LoadField { .. }
        | IlOp::BoxValue { .. }
        | IlOp::UnboxValue { .. }
        | IlOp::Return { .. }
        | IlOp::Halt { .. }
        | IlOp::LoadReturnSlot { .. }
        | IlOp::ConstReturnImm { .. }
        | IlOp::BinReturn { .. }
        | IlOp::PrologueJmp { .. } => {}
    }
    if op_blocks_length_proof(&ops[i], purity)
        && !matches!(
            &ops[i],
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { .. },
                ..
            }
        )
    {
        facts.kill_heap();
    }
}

fn apply_store(ops: &[IlOp], i: usize, dest: u32, facts: &mut Facts) {
    if i >= 1
        && is_array_len(&ops[i - 1])
        && let Some(IlOp::Load { slot: arr, .. }) = ops.get(i.saturating_sub(2))
    {
        let arr = *arr;
        facts.kill_slot(dest);
        facts.len_of.insert(dest, arr);
        return;
    }
    if let Some(IlOp::Const { imm, .. }) = ops.get(i.saturating_sub(1)) {
        facts.set_exact(dest, *imm);
        return;
    }
    if let Some(IlOp::Load { slot: src, .. }) = ops.get(i.saturating_sub(1)) {
        facts.copy_slot(dest, *src);
        return;
    }
    facts.kill_slot(dest);
}

fn apply_byte(
    ops: &[IlOp],
    i: usize,
    byte: &Byte,
    facts: &mut Facts,
    purity: Option<&PureCallCtx>,
) {
    match *byte.bytecode() {
        Instruction::ArrayLen => {}
        Instruction::StoreIndex | Instruction::StoreIndexUnchecked => {
            if let Some((arr, idx)) = store_index_operands(ops, i) {
                facts.prove_index(arr, idx);
            }
        }
        Instruction::ArrayPush | Instruction::MakeArray | Instruction::MakeDict => {
            facts.kill_heap();
        }
        Instruction::INC | Instruction::DEC => {
            let slot = byte.inc_dec_parts().0 as u32;
            let keep = facts.nonneg.contains(&slot);
            facts.kill_slot(slot);
            if keep {
                facts.nonneg.insert(slot);
            }
        }
        Instruction::CALL => {
            if !purity.is_some_and(|c| c.call_offset_is_pure(byte.call_parts().1 as u32)) {
                facts.kill_heap();
            }
        }
        Instruction::YieldCoro
        | Instruction::YieldFromCoro
        | Instruction::FORMAT
        | Instruction::FfiInvoke
        | Instruction::CallIndirect
        | Instruction::TailCall => facts.kill_all(),
        Instruction::HostInvoke
        | Instruction::PRINT
        | Instruction::GetField
        | Instruction::SetField => facts.kill_heap(),
        _ => facts.kill_all(),
    }
}

fn is_array_len(op: &IlOp) -> bool {
    match op {
        IlOp::Byte { byte, .. } => *byte.bytecode() == Instruction::ArrayLen,
        other => other
            .as_encode_byte()
            .is_some_and(|b| *b.bytecode() == Instruction::ArrayLen),
    }
}

fn is_store_index(op: &IlOp) -> bool {
    match op {
        IlOp::Byte { byte, .. } => *byte.bytecode() == Instruction::StoreIndex,
        other => other
            .as_encode_byte()
            .is_some_and(|b| *b.bytecode() == Instruction::StoreIndex),
    }
}

fn index_operands(ops: &[IlOp], i: usize) -> Option<(u32, Idx)> {
    let idx = match ops.get(i.checked_sub(1)?)? {
        IlOp::Load { slot, .. } => Idx::Slot(*slot),
        IlOp::Const { imm, .. } if *imm >= 0 => Idx::Imm(*imm),
        _ => return None,
    };
    let IlOp::Load { slot: arr, .. } = ops.get(i.checked_sub(2)?)? else {
        return None;
    };
    Some((*arr, idx))
}

fn index_only(ops: &[IlOp], i: usize) -> Option<Idx> {
    match ops.get(i.checked_sub(1)?)? {
        IlOp::Load { slot, .. } => Some(Idx::Slot(*slot)),
        IlOp::Const { imm, .. } if *imm >= 0 => Some(Idx::Imm(*imm)),
        _ => None,
    }
}

fn store_index_operands(ops: &[IlOp], i: usize) -> Option<(u32, Idx)> {
    let idx = match ops.get(i.checked_sub(2)?)? {
        IlOp::Load { slot, .. } => Idx::Slot(*slot),
        IlOp::Const { imm, .. } if *imm >= 0 => Idx::Imm(*imm),
        _ => return None,
    };
    let IlOp::Load { slot: arr, .. } = ops.get(i.checked_sub(3)?)? else {
        return None;
    };
    Some((*arr, idx))
}

fn store_pin_idx(ops: &[IlOp], i: usize) -> Option<Idx> {
    match ops.get(i.checked_sub(2)?)? {
        IlOp::Load { slot, .. } => Some(Idx::Slot(*slot)),
        IlOp::Const { imm, .. } if *imm >= 0 => Some(Idx::Imm(*imm)),
        _ => None,
    }
}

fn rewrite(ops: &mut Vec<IlOp>, before: &[Facts]) -> usize {
    let mut out = Vec::with_capacity(ops.len());
    let mut i = 0;
    let mut hits = 0usize;
    while i < ops.len() {
        if let Some((n, rewrite)) = try_rewrite(ops, i, &before[i]) {
            out.extend(rewrite);
            i += n;
            hits += 1;
            continue;
        }
        out.push(ops[i].clone());
        i += 1;
    }
    if hits > 0 {
        *ops = out;
    }
    hits
}

fn try_rewrite(ops: &[IlOp], i: usize, facts: &Facts) -> Option<(usize, Vec<IlOp>)> {
    try_index(ops, i, facts)
        .or_else(|| try_index_pin(ops, i, facts))
        .or_else(|| try_store_index(ops, i, facts))
        .or_else(|| try_store_pin(ops, i, facts))
        .or_else(|| try_known_eq(ops, i, facts))
}

fn try_index(ops: &[IlOp], i: usize, facts: &Facts) -> Option<(usize, Vec<IlOp>)> {
    let IlOp::Index { loc } = ops[i] else {
        return None;
    };
    let (arr, idx) = index_operands(ops, i)?;
    if !facts.has_index(arr, idx) {
        return None;
    }
    Some((1, vec![IlOp::IndexUnchecked { loc }]))
}

fn try_index_pin(ops: &[IlOp], i: usize, facts: &Facts) -> Option<(usize, Vec<IlOp>)> {
    let IlOp::IndexPin { slot, loc } = ops[i] else {
        return None;
    };
    let idx = index_only(ops, i)?;
    if !facts.has_index(slot, idx) {
        return None;
    }
    Some((1, vec![IlOp::IndexPinUnchecked { slot, loc }]))
}

fn try_store_index(ops: &[IlOp], i: usize, facts: &Facts) -> Option<(usize, Vec<IlOp>)> {
    if !is_store_index(&ops[i]) {
        return None;
    }
    let (arr, idx) = store_index_operands(ops, i)?;
    if !facts.has_index(arr, idx) {
        return None;
    }
    let loc = ops[i].loc();
    Some((
        1,
        vec![IlOp::from_plain_byte(
            Byte::new(Instruction::StoreIndexUnchecked),
            loc,
        )],
    ))
}

fn try_store_pin(ops: &[IlOp], i: usize, facts: &Facts) -> Option<(usize, Vec<IlOp>)> {
    let IlOp::StoreIndexPin { slot, loc } = ops[i] else {
        return None;
    };
    let idx = store_pin_idx(ops, i)?;
    if !facts.has_index(slot, idx) {
        return None;
    }
    Some((1, vec![IlOp::StoreIndexPinUnchecked { slot, loc }]))
}

fn try_known_eq(ops: &[IlOp], i: usize, facts: &Facts) -> Option<(usize, Vec<IlOp>)> {
    // LOAD s; DUP; CONST e; EQ [; JMPF/JMPT]
    if matches!(ops.get(i)?, IlOp::Load { .. })
        && matches!(ops.get(i + 1)?, IlOp::Dup { .. })
        && let IlOp::Const { imm: expected, loc: cloc } = ops.get(i + 2)?
        && matches!(ops.get(i + 3)?, IlOp::Bin { op: Instruction::EQ, .. })
    {
        let slot = match &ops[i] {
            IlOp::Load { slot, .. } => *slot,
            _ => return None,
        };
        let result = facts.eq_result(slot, *expected)?;
        let loc = *cloc;
        if let Some(IlOp::Jump {
            kind,
            target,
            loc: jloc,
            hint,
        }) = ops.get(i + 4)
        {
            let taken = match kind {
                IlJumpKind::JumpIfFalse => result == 0,
                IlJumpKind::JumpIfTrue => result != 0,
                _ => {
                    return Some((
                        4,
                        vec![
                            ops[i].clone(),
                            IlOp::Const { imm: result, loc },
                        ],
                    ));
                }
            };
            let mut out = vec![ops[i].clone()];
            if taken {
                out.push(IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: *target,
                    loc: *jloc,
                    hint: *hint,
                });
            }
            return Some((5, out));
        }
        return Some((
            4,
            vec![ops[i].clone(), IlOp::Const { imm: result, loc }],
        ));
    }
    None
}

fn block_starts_with_label(ops: &[IlOp], block: &Block, target: Label) -> bool {
    for i in block.start..block.end {
        match &ops[i] {
            IlOp::Label(l) | IlOp::JoinLabel(l) if *l == target => return true,
            IlOp::Label(_) | IlOp::JoinLabel(_) => continue,
            _ => return false,
        }
    }
    false
}

fn refine_edge(
    ops: &[IlOp],
    blocks: &[Block],
    hints: &[EdgeHint],
    pred: usize,
    succ: usize,
    base: Facts,
) -> Facts {
    match &hints[pred] {
        EdgeHint::None => base,
        EdgeHint::Cond { fall, taken, target } => {
            if block_starts_with_label(ops, &blocks[succ], *target) {
                taken.clone()
            } else {
                fall.clone()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::op::IlOp;
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    fn load(slot: u32) -> IlOp {
        IlOp::Load { slot, loc: loc() }
    }

    fn store(slot: u32) -> IlOp {
        IlOp::StorePop { slot, loc: loc() }
    }

    fn c(n: i32) -> IlOp {
        IlOp::Const { imm: n, loc: loc() }
    }

    fn jmp(kind: IlJumpKind, id: u32) -> IlOp {
        IlOp::Jump {
            kind,
            target: Label(id),
            loc: loc(),
            hint: Default::default(),
        }
    }

    fn ret() -> IlOp {
        IlOp::Return {
            loc: loc(),
            ret_words: 1,
        }
    }

    fn index() -> IlOp {
        IlOp::Index { loc: loc() }
    }

    fn store_index() -> IlOp {
        IlOp::byte(Byte::new(Instruction::StoreIndex))
    }

    #[test]
    fn second_index_becomes_unchecked() {
        let mut ops = vec![
            load(0),
            load(1),
            index(),
            load(0),
            load(1),
            index(),
            ret(),
        ];
        assert!(dominate_checks(&mut ops) >= 1);
        assert!(matches!(ops[2], IlOp::Index { .. }));
        assert!(matches!(ops[5], IlOp::IndexUnchecked { .. }));
    }

    #[test]
    fn store_index_after_index_is_unchecked() {
        let mut ops = vec![
            load(0),
            load(1),
            index(),
            IlOp::Pop { loc: loc() },
            load(0),
            load(1),
            c(0),
            store_index(),
            ret(),
        ];
        assert!(dominate_checks(&mut ops) >= 1);
        assert!(
            ops.iter().any(|op| op
                .as_encode_byte()
                .is_some_and(|b| *b.bytecode() == Instruction::StoreIndexUnchecked)),
            "dominated StoreIndex must drop the bounds check"
        );
        assert!(!ops.iter().any(|op| is_store_index(op)));
    }

    #[test]
    fn refuses_index_after_store_to_idx() {
        let mut ops = vec![
            load(0),
            load(1),
            index(),
            c(2),
            store(1),
            load(0),
            load(1),
            index(),
            ret(),
        ];
        dominate_checks(&mut ops);
        assert!(
            matches!(ops[7], IlOp::Index { .. }),
            "store to idx must kill the in-bounds fact"
        );
    }

    #[test]
    fn refuses_across_impure_call() {
        let mut ops = vec![
            load(0),
            load(1),
            index(),
            IlOp::Entry {
                kind: EntryKind::Call,
                arity: 0,
                target: Label(9),
                loc: loc(),
                ret_words: 1,
            },
            load(0),
            load(1),
            index(),
            ret(),
        ];
        dominate_checks(&mut ops);
        let checked = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Index { .. }))
            .count();
        assert_eq!(checked, 2, "CALL is a heap-fact barrier");
    }

    #[test]
    fn lt_len_guard_unchecks_index() {
        let mut ops = vec![
            c(0),
            store(1),
            load(0),
            IlOp::byte(Byte::new(Instruction::ArrayLen)),
            store(2),
            load(1),
            load(2),
            IlOp::Bin {
                op: Instruction::LE,
                loc: loc(),
            },
            jmp(IlJumpKind::JumpIfFalse, 1),
            load(0),
            load(1),
            index(),
            IlOp::Label(Label(1)),
            ret(),
        ];
        assert!(dominate_checks(&mut ops) >= 1);
        assert!(
            ops.iter().any(|op| matches!(op, IlOp::IndexUnchecked { .. })),
            "i < len(a) fall-through should prove a[i]"
        );
    }

    #[test]
    fn known_none_tag_folds_eq_jmpf() {
        let mut ops = vec![
            c(0),
            store(3),
            load(3),
            IlOp::Dup { loc: loc() },
            c(0),
            IlOp::Bin {
                op: Instruction::EQ,
                loc: loc(),
            },
            jmp(IlJumpKind::JumpIfFalse, 1),
            c(9),
            IlOp::Label(Label(1)),
            ret(),
        ];
        assert!(dominate_checks(&mut ops) >= 1);
        assert!(
            !ops.iter().any(|op| matches!(
                op,
                IlOp::Bin {
                    op: Instruction::EQ,
                    ..
                }
            )),
            "known-zero niche test should fold"
        );
        assert!(
            !ops.iter()
                .any(|op| matches!(op, IlOp::Jump { kind: IlJumpKind::JumpIfFalse, .. })),
            "EQ-true JMPF must delete"
        );
    }

    #[test]
    fn index_pin_second_is_unchecked() {
        let mut ops = vec![
            load(1),
            IlOp::IndexPin {
                slot: 0,
                loc: loc(),
            },
            load(1),
            IlOp::IndexPin {
                slot: 0,
                loc: loc(),
            },
            ret(),
        ];
        assert!(dominate_checks(&mut ops) >= 1);
        assert!(matches!(ops[1], IlOp::IndexPin { .. }));
        assert!(matches!(ops[3], IlOp::IndexPinUnchecked { .. }));
    }
}
