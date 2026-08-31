//! SSA-style global value numbering over stack IL (COI-121).
//!
//! There is no phi opcode: join disagreement on a slot gets a stable
//! `Phi(block, slot)` value number. Redundant pure binops whose result is
//! already in a slot become `Load`.

use std::collections::{HashMap, HashSet};

use common::Instruction;

use super::gvn::gvn_cfg;
use super::op::IlOp;

/// CFG + per-block slot φ and per-op produced VNs.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct SsaForm {
    /// Exclusive-end ranges of each basic block.
    pub blocks: Vec<(usize, usize)>,
    /// Predecessors of each block.
    pub preds: Vec<Vec<usize>>,
    /// Slot → VN on entry to each block (after φ).
    pub slot_in: Vec<HashMap<u32, u32>>,
    /// VN produced by the op at this index, if it pushes a numbered value.
    pub produced: Vec<Option<u32>>,
    /// Slot map immediately before each op.
    pub slots_before: Vec<HashMap<u32, u32>>,
}

/// Value numbers plus the slot map immediately before each op.
#[derive(Clone, Debug)]
pub struct ValueNumbers {
    pub produced: Vec<Option<u32>>,
    pub slots_before: Vec<HashMap<u32, u32>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum VExpr {
    Const(i32),
    ConstPool(u32),
    String(u32),
    InitSlot(u32),
    Phi { block: u32, slot: u32 },
    Bin { op: u16, a: u32, b: u32 },
    BinSlotImm { op: u16, slot: u32, imm: i16 },
    BinSlotSlot { op: u16, a: u32, b: u32 },
}

struct Intern {
    map: HashMap<VExpr, u32>,
    next: u32,
}

impl Intern {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            next: 1,
        }
    }

    fn intern(&mut self, e: VExpr) -> u32 {
        if let Some(&v) = self.map.get(&e) {
            return v;
        }
        let v = self.next;
        self.next += 1;
        self.map.insert(e, v);
        v
    }
}

fn cse_binop(op: Instruction) -> bool {
    !matches!(
        op,
        Instruction::DIV | Instruction::MOD | Instruction::DIVF | Instruction::MODF
    )
}

fn cse_binop_u8(op: u8) -> bool {
    op != Instruction::DIV as u8
        && op != Instruction::MOD as u8
        && op != Instruction::DIVF as u8
        && op != Instruction::MODF as u8
}

/// Build SSA-like slot versions and number ops.
pub fn build_ssa(ops: &[IlOp]) -> SsaForm {
    let (blocks, preds) = gvn_cfg(ops);
    let n = blocks.len();
    let mut intern = Intern::new();
    let mut slot_out: Vec<HashMap<u32, u32>> = vec![HashMap::new(); n];
    let mut stack_out: Vec<Vec<u32>> = vec![Vec::new(); n];

    for _ in 0..n.saturating_mul(2).max(4) {
        let mut changed = false;
        for bi in 0..n {
            let (slots, stack) = merge_in(&preds[bi], bi, &slot_out, &stack_out, &mut intern);
            let (so, st) =
                simulate_block(ops, blocks[bi].0, blocks[bi].1, slots, stack, &mut intern);
            if so != slot_out[bi] || st != stack_out[bi] {
                slot_out[bi] = so;
                stack_out[bi] = st;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut slot_in = vec![HashMap::new(); n];
    let mut produced = vec![None; ops.len()];
    let mut slots_before = vec![HashMap::new(); ops.len()];
    for bi in 0..n {
        let (mut slots, mut stack) = merge_in(&preds[bi], bi, &slot_out, &stack_out, &mut intern);
        slot_in[bi] = slots.clone();
        for i in blocks[bi].0..blocks[bi].1 {
            slots_before[i] = slots.clone();
            produced[i] = step(ops, i, &mut slots, &mut stack, &mut intern);
        }
    }

    SsaForm {
        blocks,
        preds,
        slot_in,
        produced,
        slots_before,
    }
}

fn merge_in(
    preds: &[usize],
    bi: usize,
    slot_out: &[HashMap<u32, u32>],
    stack_out: &[Vec<u32>],
    intern: &mut Intern,
) -> (HashMap<u32, u32>, Vec<u32>) {
    if preds.is_empty() {
        return (HashMap::new(), Vec::new());
    }
    let mut keys = Vec::new();
    for &p in preds {
        keys.extend(slot_out[p].keys().copied());
    }
    keys.sort_unstable();
    keys.dedup();
    let mut slots = HashMap::new();
    for slot in keys {
        let mut vn: Option<u32> = None;
        let mut agree = true;
        for &p in preds {
            let Some(&v) = slot_out[p].get(&slot) else {
                agree = false;
                break;
            };
            match vn {
                None => vn = Some(v),
                Some(prev) if prev == v => {}
                _ => {
                    agree = false;
                    break;
                }
            }
        }
        let v = if agree {
            vn.unwrap()
        } else {
            intern.intern(VExpr::Phi {
                block: bi as u32,
                slot,
            })
        };
        slots.insert(slot, v);
    }
    let first = &stack_out[preds[0]];
    let stack = if preds.iter().all(|&p| &stack_out[p] == first) {
        first.clone()
    } else {
        Vec::new()
    };
    (slots, stack)
}

fn simulate_block(
    ops: &[IlOp],
    start: usize,
    end: usize,
    mut slots: HashMap<u32, u32>,
    mut stack: Vec<u32>,
    intern: &mut Intern,
) -> (HashMap<u32, u32>, Vec<u32>) {
    for i in start..end {
        step(ops, i, &mut slots, &mut stack, intern);
    }
    (slots, stack)
}

fn step(
    ops: &[IlOp],
    i: usize,
    slots: &mut HashMap<u32, u32>,
    stack: &mut Vec<u32>,
    intern: &mut Intern,
) -> Option<u32> {
    match &ops[i] {
        IlOp::Label(_) | IlOp::JoinLabel(_) | IlOp::Jump { .. } => None,
        IlOp::Const { imm, .. } => {
            let v = intern.intern(VExpr::Const(*imm));
            stack.push(v);
            Some(v)
        }
        IlOp::ConstPool { idx, .. } => {
            let v = intern.intern(VExpr::ConstPool(*idx));
            stack.push(v);
            Some(v)
        }
        IlOp::String { idx, .. } => {
            let v = intern.intern(VExpr::String(*idx));
            stack.push(v);
            Some(v)
        }
        IlOp::Load { slot, .. } => {
            let v = slots
                .get(slot)
                .copied()
                .unwrap_or_else(|| intern.intern(VExpr::InitSlot(*slot)));
            slots.entry(*slot).or_insert(v);
            stack.push(v);
            Some(v)
        }
        IlOp::Dup { .. } => {
            if let Some(&t) = stack.last() {
                stack.push(t);
                Some(t)
            } else {
                None
            }
        }
        IlOp::Pop { .. } => {
            stack.pop();
            None
        }
        IlOp::StorePop { slot, .. } => {
            if let Some(v) = stack.pop() {
                slots.insert(*slot, v);
            } else {
                stack.clear();
            }
            None
        }
        IlOp::Bin { op, .. } if cse_binop(*op) => {
            let b = stack.pop();
            let a = stack.pop();
            match (a, b) {
                (Some(a), Some(b)) => {
                    let v = intern.intern(VExpr::Bin {
                        op: *op as u16,
                        a,
                        b,
                    });
                    stack.push(v);
                    Some(v)
                }
                _ => {
                    stack.clear();
                    None
                }
            }
        }
        IlOp::BinSlotImm { op, slot, imm, .. } if cse_binop_u8(*op) => {
            let sv = slots
                .get(&u32::from(*slot))
                .copied()
                .unwrap_or_else(|| intern.intern(VExpr::InitSlot(u32::from(*slot))));
            let v = intern.intern(VExpr::BinSlotImm {
                op: *op as u16,
                slot: sv,
                imm: *imm,
            });
            stack.push(v);
            Some(v)
        }
        IlOp::BinSlotSlot { op, a, b, .. } if cse_binop_u8(*op) => {
            let av = slots
                .get(&u32::from(*a))
                .copied()
                .unwrap_or_else(|| intern.intern(VExpr::InitSlot(u32::from(*a))));
            let bv = slots
                .get(&u32::from(*b))
                .copied()
                .unwrap_or_else(|| intern.intern(VExpr::InitSlot(u32::from(*b))));
            let v = intern.intern(VExpr::BinSlotSlot {
                op: *op as u16,
                a: av,
                b: bv,
            });
            stack.push(v);
            Some(v)
        }
        IlOp::Return { .. }
        | IlOp::Halt { .. }
        | IlOp::LoadReturnSlot { .. }
        | IlOp::ConstReturnImm { .. }
        | IlOp::BinReturn { .. } => {
            stack.clear();
            None
        }
        _ => {
            stack.clear();
            None
        }
    }
}

/// Number values from an SSA form.
pub fn number_values(ssa: &SsaForm) -> ValueNumbers {
    ValueNumbers {
        produced: ssa.produced.clone(),
        slots_before: ssa.slots_before.clone(),
    }
}

/// Replace `Load`/`Const` + `Bin` with `Load slot` when the result VN is
/// already in `slot`.
pub fn eliminate_redundant(ops: &mut Vec<IlOp>, value_numbers: &ValueNumbers) {
    if ops.len() < 3 || value_numbers.produced.len() != ops.len() {
        return;
    }
    let produced = &value_numbers.produced;
    let slots_before = &value_numbers.slots_before;
    if slots_before.len() != ops.len() {
        return;
    }

    let mut drop = HashSet::new();
    let mut i = 2;
    while i < ops.len() {
        if drop.contains(&i) {
            i += 1;
            continue;
        }
        if !matches!(&ops[i], IlOp::Bin { op, .. } if cse_binop(*op)) {
            i += 1;
            continue;
        }
        let Some(vn) = produced[i] else {
            i += 1;
            continue;
        };
        if !is_bin_operand(&ops[i - 2]) || !is_bin_operand(&ops[i - 1]) {
            i += 1;
            continue;
        }
        let Some((&slot, _)) = slots_before[i - 2].iter().find(|(_, v)| **v == vn) else {
            i += 1;
            continue;
        };
        let loc = ops[i].loc();
        ops[i] = IlOp::Load { slot, loc };
        drop.insert(i - 2);
        drop.insert(i - 1);
        i += 1;
    }
    if drop.is_empty() {
        return;
    }
    let mut out = Vec::with_capacity(ops.len() - drop.len());
    for (idx, op) in ops.iter().enumerate() {
        if !drop.contains(&idx) {
            out.push(op.clone());
        }
    }
    *ops = out;
}

fn is_bin_operand(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Load { .. } | IlOp::Const { .. } | IlOp::ConstPool { .. }
    )
}

/// Build SSA, number, and rewrite redundant pure binops.
pub fn ssa_gvn(ops: &mut Vec<IlOp>) {
    if ops.len() < 3 {
        return;
    }
    let ssa = build_ssa(ops);
    eliminate_redundant(ops, &number_values(&ssa));
}
