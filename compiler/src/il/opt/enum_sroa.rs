//! Scalar replacement of local enums.
//!
//! A slot whose every reaching definition is `MakeEnum; STORE s` and whose
//! every read is a match dispatch (`LOAD s; JumpIfMatch*; Unpack | POP`)
//! becomes a tag slot plus payload slots shared by all variants:
//!
//! - a site stores each payload word as it is computed, then the tag;
//! - a dispatch compares the tag and stores the payload where the VM would
//!   have written it (the scrutinee's slot upward), so arms are unchanged.
//!
//! Fail-closed: enums with `fn drop()` (`TagEnumType` after `MakeEnum`),
//! whole-value uses (calls, returns, bindings, equality), unknown cursors,
//! and any `Seek` that could expose the fresh slots to operand pushes while
//! the value is still live all keep the heap enum.

use std::collections::{BTreeSet, HashMap};

use common::Instruction;

use super::super::analysis::{Block, build_blocks, preds_of};
use super::super::op::{IlJumpKind, IlOp, Label};
use super::escape_analysis::{
    SlotTouch, element_starts, max_slot_used, owned_loads, slot_touch,
};

/// Largest payload a scalarized enum may carry.
const MAX_PAYLOAD: u32 = 8;

struct Site {
    make_idx: usize,
    tag: u16,
    arity: u32,
    /// Payload code starts in push order (payload[arity - 1] first).
    elem_starts: Vec<usize>,
}

enum Terminal {
    Unpack(u32),
    Pop,
}

struct Dispatch {
    load_idx: usize,
    /// `(tag, arity, target)` per `JumpIfMatch`.
    arms: Vec<(u32, u32, Label)>,
    term: Terminal,
    term_idx: usize,
    /// Scrutinee slot: the cursor before the `LOAD`.
    base: u32,
}

struct Plan {
    sites: Vec<Site>,
    dispatches: Vec<Dispatch>,
    tags: BTreeSet<u32>,
    width: u32,
}

/// Rewrite every scalarizable local enum. Returns how many slots changed.
pub fn scalarize_enums(ops: &mut Vec<IlOp>, entry_tell: u32, next_label: &mut u32) -> usize {
    let mut by_slot: HashMap<u32, Vec<Site>> = HashMap::new();
    for i in 0..ops.len().saturating_sub(1) {
        let IlOp::MakeEnum { tag, arity, .. } = ops[i] else {
            continue;
        };
        let IlOp::StorePop { slot, .. } = ops[i + 1] else {
            continue;
        };
        let arity = u32::from(arity);
        let elem_starts = if arity == 0 {
            Some(Vec::new())
        } else if arity <= MAX_PAYLOAD {
            element_starts(ops, i, arity)
        } else {
            None
        };
        let entry = by_slot.entry(slot).or_default();
        match elem_starts {
            Some(elem_starts) => entry.push(Site {
                make_idx: i,
                tag,
                arity,
                elem_starts,
            }),
            // One unsplittable site poisons the slot.
            None => entry.push(Site {
                make_idx: usize::MAX,
                tag,
                arity,
                elem_starts: Vec::new(),
            }),
        }
    }
    if by_slot.is_empty() || ops.iter().any(is_unpack_at) {
        return 0;
    }

    let blocks = build_blocks(ops);
    let tells = super::super::tell::analyze_il_at(ops, entry_tell);
    let mut slots: Vec<u32> = by_slot.keys().copied().collect();
    slots.sort_unstable();
    let mut plans: Vec<(u32, Plan)> = Vec::new();
    for slot in slots {
        let sites = by_slot.remove(&slot).expect("key");
        if sites.iter().any(|s| s.make_idx == usize::MAX) {
            continue;
        }
        if let Some(plan) = plan_slot(ops, &blocks, &tells, slot, sites) {
            plans.push((slot, plan));
        }
    }
    if plans.is_empty() {
        return 0;
    }

    // Fresh slots above every named local, `Seek` floor and payload landing.
    let mut base = max_slot_used(ops);
    for op in ops.iter() {
        if let Some(k) = seek_operand(op) {
            base = base.max(k);
        }
    }
    for (_, p) in &plans {
        for d in &p.dispatches {
            base = base.max(d.base + p.width);
        }
    }
    base += 1;

    // Assign tag slot `t` and payload slots `t + 1 ..` per plan; then check
    // every hazard `Seek` against the final layout.
    let mut layout: Vec<(u32, u32)> = Vec::new();
    let mut next = base;
    for (_, p) in &plans {
        layout.push((next, next + 1));
        next += 1 + p.width;
    }
    if next > 256 {
        return 0;
    }
    let preds = preds_of(&blocks);
    let mut kept = Vec::new();
    for ((slot, p), (tag_slot, _)) in plans.into_iter().zip(layout) {
        let top = tag_slot + p.width;
        if seek_hazard(ops, &blocks, &preds, slot, &p, top) {
            continue;
        }
        kept.push((p, tag_slot));
    }
    if kept.is_empty() {
        return 0;
    }
    let n = kept.len();
    rewrite(ops, &kept, next_label);
    n
}

fn plan_slot(
    ops: &[IlOp],
    blocks: &[Block],
    tells: &super::super::tell::TellInfo,
    slot: u32,
    sites: Vec<Site>,
) -> Option<Plan> {
    let stores: Vec<usize> = sites.iter().map(|s| s.make_idx + 1).collect();
    let owned = owned_loads(ops, blocks, &stores, slot)?;
    if owned.is_empty() {
        return None;
    }
    let tags: BTreeSet<u32> = sites.iter().map(|s| u32::from(s.tag)).collect();
    let mut width = sites.iter().map(|s| s.arity).max().unwrap_or(0);
    let mut dispatches = Vec::with_capacity(owned.len());
    for &load_idx in &owned {
        let d = parse_dispatch(ops, load_idx, tells)?;
        for &(_, a, _) in &d.arms {
            width = width.max(a);
        }
        if let Terminal::Unpack(a) = d.term {
            width = width.max(a);
        }
        dispatches.push(d);
    }
    if width > MAX_PAYLOAD {
        return None;
    }
    Some(Plan {
        sites,
        dispatches,
        tags,
        width,
    })
}

fn parse_dispatch(
    ops: &[IlOp],
    load_idx: usize,
    tells: &super::super::tell::TellInfo,
) -> Option<Dispatch> {
    let base = tells.tell_before(load_idx).known()?;
    let mut arms = Vec::new();
    let mut j = load_idx + 1;
    while let Some(IlOp::Jump {
        kind: IlJumpKind::JumpIfMatch { tag, arity },
        target,
        ..
    }) = ops.get(j)
    {
        arms.push((*tag, *arity, *target));
        j += 1;
    }
    let term = match ops.get(j)? {
        IlOp::Pop { .. } => Terminal::Pop,
        op => {
            let b = op.as_plain_byte()?;
            match *b.bytecode() {
                Instruction::Unpack => Terminal::Unpack(b.operand_u32()),
                Instruction::POP => Terminal::Pop,
                _ => return None,
            }
        }
    };
    Some(Dispatch {
        load_idx,
        arms,
        term,
        term_idx: j,
        base,
    })
}

fn is_unpack_at(op: &IlOp) -> bool {
    matches!(op, IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::UnpackAt)
}

fn seek_operand(op: &IlOp) -> Option<u32> {
    match op {
        IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Seek => Some(byte.operand_u32()),
        _ => None,
    }
}

/// Per-op "value of `slot` still needed" (live-in), counting only the
/// plan's dispatch loads as uses and any write of `slot` as a kill.
fn live_in(ops: &[IlOp], blocks: &[Block], preds: &[Vec<usize>], slot: u32, p: &Plan) -> Vec<bool> {
    let uses: BTreeSet<usize> = p.dispatches.iter().map(|d| d.load_idx).collect();
    let n = blocks.len();
    let mut succs = vec![Vec::new(); n];
    for (b, ps) in preds.iter().enumerate() {
        for &q in ps {
            succs[q].push(b);
        }
    }
    let mut block_in = vec![false; n];
    let mut live = vec![false; ops.len()];
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..n).rev() {
            let mut l = succs[b].iter().any(|&s| block_in[s]);
            for i in (blocks[b].start..blocks[b].end).rev() {
                if uses.contains(&i) {
                    l = true;
                } else if slot_touch(&ops[i], slot) == SlotTouch::Write {
                    l = false;
                }
                live[i] = l;
            }
            if l != block_in[b] {
                block_in[b] = l;
                changed = true;
            }
        }
    }
    live
}

/// A `Seek` at or below the fresh slots resets the cursor under them; the
/// operand pushes that follow would overwrite them. Allowed only when the
/// value is dead after the `Seek`, or the `Seek` opens a dispatch that is
/// the value's last use.
fn seek_hazard(
    ops: &[IlOp],
    blocks: &[Block],
    preds: &[Vec<usize>],
    slot: u32,
    p: &Plan,
    top: u32,
) -> bool {
    let live = live_in(ops, blocks, preds, slot, p);
    let label_idx: HashMap<Label, usize> = ops
        .iter()
        .enumerate()
        .filter_map(|(i, op)| match op {
            IlOp::Label(l) | IlOp::JoinLabel(l) => Some((*l, i)),
            _ => None,
        })
        .collect();
    let live_at = |i: usize| live.get(i).copied().unwrap_or(false);
    for (q, op) in ops.iter().enumerate() {
        let Some(k) = seek_operand(op) else {
            continue;
        };
        if k > top || !live_at(q + 1) {
            continue;
        }
        let Some(d) = p.dispatches.iter().find(|d| d.load_idx == q + 1) else {
            return true;
        };
        let after_arms = d
            .arms
            .iter()
            .any(|(_, _, t)| label_idx.get(t).is_none_or(|&i| live_at(i)));
        if after_arms || live_at(d.term_idx + 1) {
            return true;
        }
    }
    false
}

fn rewrite(ops: &mut Vec<IlOp>, kept: &[(Plan, u32)], next_label: &mut u32) {
    // Element stores keyed by the op they precede.
    let mut before: HashMap<usize, u32> = HashMap::new();
    let mut site_at: HashMap<usize, (u16, u32, u32)> = HashMap::new(); // make_idx → (tag, arity, t)
    let mut dispatch_at: HashMap<usize, (&Dispatch, &Plan, u32)> = HashMap::new();
    for (p, t) in kept {
        for s in &p.sites {
            // Push order m holds payload[arity - 1 - m].
            for m in 1..s.elem_starts.len() {
                before.insert(s.elem_starts[m], t + 1 + (s.arity - m as u32));
            }
            site_at.insert(s.make_idx, (s.tag, s.arity, *t));
        }
        for d in &p.dispatches {
            dispatch_at.insert(d.load_idx, (d, p, *t));
        }
    }

    let mut out = Vec::with_capacity(ops.len() + 16);
    let mut i = 0;
    while i < ops.len() {
        if let Some(&slot) = before.get(&i) {
            out.push(IlOp::StorePop {
                slot,
                loc: ops[i].loc(),
            });
        }
        if let Some(&(tag, arity, t)) = site_at.get(&i) {
            let loc = ops[i].loc();
            if arity > 0 {
                out.push(IlOp::StorePop { slot: t + 1, loc });
            }
            out.push(IlOp::Const {
                imm: i32::from(tag),
                loc,
            });
            out.push(IlOp::StorePop { slot: t, loc });
            i += 2;
            continue;
        }
        if let Some(&(d, p, t)) = dispatch_at.get(&i) {
            let loc = ops[i].loc();
            let bind = |out: &mut Vec<IlOp>, n: u32| {
                for j in 0..n {
                    out.push(IlOp::Load { slot: t + 1 + j, loc });
                    out.push(IlOp::StorePop {
                        slot: d.base + j,
                        loc,
                    });
                }
            };
            for &(tag, arity, target) in &d.arms {
                if !p.tags.contains(&tag) {
                    continue;
                }
                let miss = Label(*next_label);
                *next_label += 1;
                out.push(IlOp::Load { slot: t, loc });
                out.push(IlOp::Const {
                    imm: tag as i32,
                    loc,
                });
                out.push(IlOp::Bin {
                    op: Instruction::EQ,
                    loc,
                });
                out.push(IlOp::jump(IlJumpKind::JumpIfFalse, miss, loc));
                bind(&mut out, arity);
                out.push(IlOp::jump(IlJumpKind::Unconditional, target, loc));
                out.push(IlOp::Label(miss));
            }
            if let Terminal::Unpack(n) = d.term {
                bind(&mut out, n);
            }
            i = d.term_idx + 1;
            continue;
        }
        out.push(ops[i].clone());
        i += 1;
    }
    *ops = out;
}

#[cfg(test)]
#[path = "enum_sroa.tests.rs"]
mod tests;
