//! Lite induction-variable strength reduction (heuristic — no PGO).
//!
//! Rewrites integer `i * c` into an add recurrence when the loop has a
//! proven unit or invariant-stride IV. Float `cast(i)` affine forms are
//! refused: a recurrence is not IEEE-exact vs `a * (i as float) + b`.
//! Fail-closed: stores to the factor, impure calls, host, FFI, yield,
//! length-changing ops, and quadratic / div-by-IV float forms stay as-is.

use std::collections::HashSet;

use common::{DebugLoc, Instruction};

use super::analysis::{NaturalLoop, find_natural_loops};
use super::licm::{insert_preheader_ops, loop_has_barrier, slots_stored_in_loop};
use super::op::{IlJumpKind, IlOp};
use super::pure_call::{PureCallCtx, op_blocks_length_proof};
use super::sp;

/// Apply lite SR. Returns the number of sites rewritten.
pub fn strength_reduce(
    ops: &mut Vec<IlOp>,
    pool: &mut Vec<u64>,
    purity: Option<&PureCallCtx>,
) -> usize {
    if ops.len() < 8 {
        return 0;
    }
    let mut n = 0usize;
    for _ in 0..16 {
        if !strength_reduce_once(ops, pool, purity) {
            break;
        }
        n += 1;
    }
    n
}

#[derive(Clone, Copy, Debug)]
enum Step {
    Imm(i32),
    Slot(u32),
}

struct Induction {
    slot: u32,
    step: Step,
    header_label: super::op::Label,
}

fn strength_reduce_once(
    ops: &mut Vec<IlOp>,
    pool: &mut Vec<u64>,
    purity: Option<&PureCallCtx>,
) -> bool {
    let info = sp::analyze(ops);
    let mut loops = find_natural_loops(ops);
    loops.sort_by_key(|l| std::cmp::Reverse(l.header));
    for lp in loops {
        if !info.sp_before(lp.header).is_known() {
            continue;
        }
        if loop_has_barrier(ops, &lp, purity) || loop_has_sr_barrier(ops, &lp, purity) {
            continue;
        }
        let Some(iv) = find_induction(ops, &lp) else {
            continue;
        };
        let stored = slots_stored_in_loop(ops, &lp);
        if let Some((start, end)) = find_sr_site(ops, &lp, iv.slot, &stored) {
            return apply_sr(ops, pool, &lp, &iv, start, end);
        }
    }
    false
}

fn loop_has_sr_barrier(ops: &[IlOp], lp: &NaturalLoop, purity: Option<&PureCallCtx>) -> bool {
    (lp.header..=lp.latch).any(|i| {
        let op = &ops[i];
        if op_blocks_length_proof(op, purity) {
            return true;
        }
        match op {
            IlOp::MakeArray { .. } => true,
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::ArrayPush | Instruction::MakeArray
                ) =>
            {
                true
            }
            other if other.as_encode_byte().is_some_and(|b| {
                matches!(
                    *b.bytecode(),
                    Instruction::ArrayPush | Instruction::MakeArray
                )
            }) =>
            {
                true
            }
            _ => false,
        }
    })
}

fn find_induction(ops: &[IlOp], lp: &NaturalLoop) -> Option<Induction> {
    let (idx, _) = header_lt_bound(ops, lp)?;
    let stored = slots_stored_in_loop(ops, lp);
    let mut step: Option<Step> = None;
    let mut stores = 0usize;
    let mut i = lp.body_start();
    while i < lp.latch {
        if let IlOp::StorePop { slot, .. } = &ops[i]
            && *slot == idx
        {
            stores += 1;
            step = step_before_store(ops, i, idx, &stored);
            if step.is_none() {
                return None;
            }
        }
        i += 1;
    }
    if stores != 1 {
        return None;
    }
    Some(Induction {
        slot: idx,
        step: step?,
        header_label: lp.header_label,
    })
}

fn header_lt_bound(ops: &[IlOp], lp: &NaturalLoop) -> Option<(u32, u32)> {
    let mut i = lp.body_start();
    while i + 1 < lp.latch {
        if let IlOp::Load { slot: a, .. } = &ops[i]
            && i + 3 < lp.latch
            && let IlOp::Load { slot: b, .. } = &ops[i + 1]
            && is_cmp(&ops[i + 2], Instruction::LE)
            && is_jmpf(&ops[i + 3])
        {
            return Some((*a, *b));
        }
        if let IlOp::Load { slot: a, .. } = &ops[i]
            && i + 3 < lp.latch
            && let IlOp::Load { slot: b, .. } = &ops[i + 1]
            && is_cmp(&ops[i + 2], Instruction::GT)
            && is_jmpf(&ops[i + 3])
        {
            return Some((*b, *a));
        }
        if let IlOp::BinSlotSlot { op, a, b, .. } = &ops[i]
            && i + 1 < lp.latch
            && is_jmpf(&ops[i + 1])
        {
            if *op == Instruction::LE as u8 {
                return Some((*a as u32, *b as u32));
            }
            if *op == Instruction::GT as u8 {
                return Some((*b as u32, *a as u32));
            }
        }
        i += 1;
    }
    None
}

fn is_cmp(op: &IlOp, want: Instruction) -> bool {
    matches!(op, IlOp::Bin { op, .. } if *op == want)
        || op.as_encode_byte().is_some_and(|b| *b.bytecode() == want)
}

fn is_jmpf(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            ..
        }
    )
}

fn step_before_store(ops: &[IlOp], store_i: usize, iv: u32, stored: &HashSet<u32>) -> Option<Step> {
    if store_i >= 3
        && matches!(&ops[store_i - 1], IlOp::Bin { op: Instruction::ADD, .. })
        && matches!(&ops[store_i - 2], IlOp::Const { imm, .. } if *imm > 0)
        && matches!(&ops[store_i - 3], IlOp::Load { slot, .. } if *slot == iv)
    {
        if let IlOp::Const { imm, .. } = &ops[store_i - 2] {
            return Some(Step::Imm(*imm));
        }
    }
    if store_i >= 1
        && let IlOp::BinSlotImm {
            op,
            slot,
            imm,
            ..
        } = &ops[store_i - 1]
        && *op == Instruction::ADD as u8
        && *slot as u32 == iv
        && *imm > 0
    {
        return Some(Step::Imm(i32::from(*imm)));
    }
    if store_i >= 3 && matches!(&ops[store_i - 1], IlOp::Bin { op: Instruction::ADD, .. }) {
        let IlOp::Load { slot: a, .. } = &ops[store_i - 2] else {
            return None;
        };
        let IlOp::Load { slot: b, .. } = &ops[store_i - 3] else {
            return None;
        };
        let other = if *a == iv && *b != iv {
            *b
        } else if *b == iv && *a != iv {
            *a
        } else {
            return None;
        };
        if stored.contains(&other) {
            return None;
        }
        return Some(Step::Slot(other));
    }
    if store_i >= 1
        && let IlOp::BinSlotSlot { op, a, b, .. } = &ops[store_i - 1]
        && *op == Instruction::ADD as u8
    {
        let a = *a as u32;
        let b = *b as u32;
        let other = if a == iv && b != iv {
            b
        } else if b == iv && a != iv {
            a
        } else {
            return None;
        };
        if stored.contains(&other) {
            return None;
        }
        return Some(Step::Slot(other));
    }
    None
}

/// `(start, end)` for one integer `iv * invariant` site.
fn find_sr_site(
    ops: &[IlOp],
    lp: &NaturalLoop,
    iv: u32,
    stored: &HashSet<u32>,
) -> Option<(usize, usize)> {
    let mut i = lp.body_start();
    while i < lp.latch {
        if let Some(end) = match_int_mul(ops, i, lp.latch, iv, stored) {
            return Some((i, end));
        }
        i += 1;
    }
    None
}

fn match_int_mul(
    ops: &[IlOp],
    i: usize,
    latch: usize,
    iv: u32,
    stored: &HashSet<u32>,
) -> Option<usize> {
    if i + 2 < latch
        && matches!(&ops[i], IlOp::Load { slot, .. } if *slot == iv)
        && is_invariant_int_factor(&ops[i + 1], iv, stored)
        && is_int_mul(&ops[i + 2])
    {
        return Some(i + 3);
    }
    if i + 2 < latch
        && is_invariant_int_factor(&ops[i], iv, stored)
        && matches!(&ops[i + 1], IlOp::Load { slot, .. } if *slot == iv)
        && is_int_mul(&ops[i + 2])
    {
        return Some(i + 3);
    }
    if let IlOp::BinSlotImm { op, slot, .. } = &ops[i]
        && *op == Instruction::MUL as u8
        && *slot as u32 == iv
    {
        return Some(i + 1);
    }
    if let IlOp::BinSlotSlot { op, a, b, .. } = &ops[i]
        && *op == Instruction::MUL as u8
    {
        let a = *a as u32;
        let b = *b as u32;
        if (a == iv && b != iv && !stored.contains(&b))
            || (b == iv && a != iv && !stored.contains(&a))
        {
            return Some(i + 1);
        }
    }
    None
}

fn is_invariant_int_factor(op: &IlOp, iv: u32, stored: &HashSet<u32>) -> bool {
    match op {
        IlOp::Const { .. } | IlOp::ConstPool { .. } => true,
        IlOp::Load { slot, .. } => *slot != iv && !stored.contains(slot),
        _ => false,
    }
}

fn is_int_mul(op: &IlOp) -> bool {
    matches!(op, IlOp::Bin { op: Instruction::MUL, .. })
        || op
            .as_encode_byte()
            .is_some_and(|b| *b.bytecode() == Instruction::MUL)
}

fn apply_sr(
    ops: &mut Vec<IlOp>,
    _pool: &mut Vec<u64>,
    lp: &NaturalLoop,
    iv: &Induction,
    start: usize,
    end: usize,
) -> bool {
    let loc = ops[start].loc();
    let mut next = max_slot_used(ops).saturating_add(1);
    if next + 2 > 250 {
        return false;
    }
    let acc = next;
    next += 1;
    let delta = next;
    next += 1;
    let iv_tmp = next;

    let chain: Vec<IlOp> = ops[start..end].to_vec();
    let add_op = Instruction::ADD;
    let sub_op = Instruction::SUB;

    let mut pre = Vec::new();
    pre.extend(chain.iter().cloned());
    pre.push(IlOp::StorePop { slot: acc, loc });
    pre.push(IlOp::Load {
        slot: iv.slot,
        loc,
    });
    pre.extend(step_add_ops(iv.step, loc));
    pre.push(IlOp::StorePop { slot: iv_tmp, loc });
    pre.extend(rewrite_iv_loads(&chain, iv.slot, iv_tmp));
    pre.push(IlOp::Load { slot: acc, loc });
    pre.push(IlOp::Bin { op: sub_op, loc });
    pre.push(IlOp::StorePop { slot: delta, loc });

    ops.splice(
        start..end,
        std::iter::once(IlOp::Load { slot: acc, loc }),
    );

    let Some(lp2) = find_natural_loops(ops)
        .into_iter()
        .find(|l| l.header_label == iv.header_label)
    else {
        return false;
    };
    let Some(store_i) = find_iv_store(ops, &lp2, iv.slot) else {
        return false;
    };
    let update = [
        IlOp::Load { slot: acc, loc },
        IlOp::Load { slot: delta, loc },
        IlOp::Bin { op: add_op, loc },
        IlOp::StorePop { slot: acc, loc },
    ];
    let at = store_i + 1;
    for (k, op) in update.into_iter().enumerate() {
        ops.insert(at + k, op);
    }

    let Some(lp3) = find_natural_loops(ops)
        .into_iter()
        .find(|l| l.header_label == iv.header_label)
    else {
        return false;
    };
    let _ = lp;
    insert_preheader_ops(ops, &lp3, pre);
    true
}

fn step_add_ops(step: Step, loc: DebugLoc) -> Vec<IlOp> {
    match step {
        Step::Imm(k) => {
            vec![
                IlOp::Const { imm: k, loc },
                IlOp::Bin {
                    op: Instruction::ADD,
                    loc,
                },
            ]
        }
        Step::Slot(s) => {
            vec![
                IlOp::Load { slot: s, loc },
                IlOp::Bin {
                    op: Instruction::ADD,
                    loc,
                },
            ]
        }
    }
}

fn rewrite_iv_loads(chain: &[IlOp], iv: u32, tmp: u32) -> Vec<IlOp> {
    chain
        .iter()
        .map(|op| match op {
            IlOp::Load { slot, loc } if *slot == iv => IlOp::Load { slot: tmp, loc: *loc },
            IlOp::BinSlotImm {
                op: bop,
                slot,
                imm,
                loc,
            } if *slot as u32 == iv && tmp <= u8::MAX as u32 => IlOp::BinSlotImm {
                op: *bop,
                slot: tmp as u8,
                imm: *imm,
                loc: *loc,
            },
            IlOp::BinSlotSlot { op: bop, a, b, loc } => {
                let a = if *a as u32 == iv { tmp as u8 } else { *a };
                let b = if *b as u32 == iv { tmp as u8 } else { *b };
                IlOp::BinSlotSlot {
                    op: *bop,
                    a,
                    b,
                    loc: *loc,
                }
            }
            other => other.clone(),
        })
        .collect()
}

fn find_iv_store(ops: &[IlOp], lp: &NaturalLoop, iv: u32) -> Option<usize> {
    (lp.body_start()..lp.latch).find(|&i| matches!(&ops[i], IlOp::StorePop { slot, .. } if *slot == iv))
}

fn max_slot_used(ops: &[IlOp]) -> u32 {
    let mut max = 0u32;
    for op in ops {
        match op {
            IlOp::Load { slot, .. } | IlOp::StorePop { slot, .. } => max = max.max(*slot),
            IlOp::BinSlotImm { slot, .. } => max = max.max(*slot as u32),
            IlOp::BinSlotSlot { a, b, .. } => {
                max = max.max(*a as u32).max(*b as u32);
            }
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::LOAD | Instruction::STORE | Instruction::StorePop
                ) =>
            {
                for k in 0..byte.load_store_count() {
                    max = max.max(byte.load_store_slot_at(k));
                }
            }
            _ => {}
        }
    }
    max
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::op::Label;
    use common::Byte;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    fn counted_mul_loop() -> Vec<IlOp> {
        vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop { slot: 0, loc: loc() },
            IlOp::Const { imm: 10, loc: loc() },
            IlOp::StorePop { slot: 1, loc: loc() },
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::StorePop { slot: 2, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::Load { slot: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::LE,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::Load { slot: 2, loc: loc() },
            IlOp::Bin {
                op: Instruction::MUL,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::StorePop { slot: 0, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Halt { loc: loc() },
        ]
    }

    fn body_muls(ops: &[IlOp]) -> usize {
        let header = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(0))))
            .unwrap();
        let latch = ops
            .iter()
            .rposition(|op| {
                matches!(
                    op,
                    IlOp::Jump {
                        kind: IlJumpKind::Unconditional,
                        target: Label(0),
                        ..
                    }
                )
            })
            .unwrap();
        ops[header..latch]
            .iter()
            .filter(|op| matches!(op, IlOp::Bin { op: Instruction::MUL, .. }))
            .count()
    }

    #[test]
    fn reduces_iv_times_invariant() {
        let mut ops = counted_mul_loop();
        let mut pool = Vec::new();
        assert_eq!(strength_reduce(&mut ops, &mut pool, None), 1);
        assert_eq!(body_muls(&ops), 0, "i*c must leave the loop body");
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::Bin { op: Instruction::ADD, .. })),
            "recurrence should add the stride product"
        );
    }

    #[test]
    fn refuses_host_invoke() {
        let mut ops = counted_mul_loop();
        ops.insert(
            15,
            IlOp::HostInvoke {
                arity: 0,
                layout: 0,
                loc: loc(),
            },
        );
        let before_len = ops.len();
        let mut pool = Vec::new();
        assert_eq!(strength_reduce(&mut ops, &mut pool, None), 0);
        assert_eq!(ops.len(), before_len);
        assert_eq!(body_muls(&ops), 1);
    }

    #[test]
    fn refuses_array_push() {
        let mut ops = counted_mul_loop();
        ops.insert(15, IlOp::byte(Byte::new(Instruction::ArrayPush)));
        let mut pool = Vec::new();
        assert_eq!(strength_reduce(&mut ops, &mut pool, None), 0);
        assert_eq!(body_muls(&ops), 1);
    }

    #[test]
    fn refuses_non_additive_iv() {
        let mut ops = counted_mul_loop();
        // Replace i += 1 with i = i * 2.
        let add_at = ops
            .iter()
            .position(|op| matches!(op, IlOp::Bin { op: Instruction::ADD, .. }))
            .unwrap();
        ops[add_at] = IlOp::Bin {
            op: Instruction::MUL,
            loc: loc(),
        };
        let mut pool = Vec::new();
        assert_eq!(strength_reduce(&mut ops, &mut pool, None), 0);
    }

    #[test]
    fn refuses_float_cast_of_iv() {
        // Recurrence of `i as float` is not IEEE-equal to the original cast.
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop { slot: 0, loc: loc() },
            IlOp::Const { imm: 4, loc: loc() },
            IlOp::StorePop { slot: 1, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::Load { slot: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::LE,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::byte(Byte::new(Instruction::CastIntToFloat)),
            IlOp::Pop { loc: loc() },
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::StorePop { slot: 0, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Halt { loc: loc() },
        ];
        let mut pool = Vec::new();
        assert_eq!(strength_reduce(&mut ops, &mut pool, None), 0);
    }

    #[test]
    fn reduces_bin_slot_imm_mul() {
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop { slot: 0, loc: loc() },
            IlOp::Const { imm: 8, loc: loc() },
            IlOp::StorePop { slot: 1, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::Load { slot: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::LE,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::BinSlotImm {
                op: Instruction::MUL as u8,
                slot: 0,
                imm: 5,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 0,
                imm: 1,
                loc: loc(),
            },
            IlOp::StorePop { slot: 0, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Halt { loc: loc() },
        ];
        let mut pool = Vec::new();
        assert_eq!(strength_reduce(&mut ops, &mut pool, None), 1);
        let header = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(0))))
            .unwrap();
        let latch = ops
            .iter()
            .rposition(|op| {
                matches!(
                    op,
                    IlOp::Jump {
                        kind: IlJumpKind::Unconditional,
                        target: Label(0),
                        ..
                    }
                )
            })
            .unwrap();
        assert!(
            !ops[header..latch].iter().any(|op| matches!(
                op,
                IlOp::BinSlotImm {
                    op,
                    ..
                } if *op == Instruction::MUL as u8
            )),
            "BinSlotImm MUL of the IV must leave the loop body"
        );
    }
}
