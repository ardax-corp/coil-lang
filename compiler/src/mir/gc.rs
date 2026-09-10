//! I5 / S2a — alloc edges and live-root sidecar at MIR GC safepoints.
//!
//! `MakeArray` / `MakeTuple` / `MakeEnum` / `InitTyped` lower to
//! [`crate::mir::MirInst::Alloc`] plus a [`crate::mir::MirInst::GcBarrier`]
//! safepoint. `ArrayPush` lowers to [`crate::mir::MirInst::ArrayPush`]
//! plus a barrier (B6 grow). `FORMAT` / `STRINGIFY` pair the same way
//! (Q9 R3). Heap `GetField` / `SetField` / `LoadField` lower on the map
//! path so InitTyped+field drafts bind (D1); they are not alloc sites.
//! [`fill_live_roots`] records live heap-word SSA values (and IL
//! slots when the builder snapshotted them). Dense specialize and MIR→LIR
//! sidecar. S2c may emit dense / LIR across alloc when S2b maps exist.
//!
//! Stack-map roadmap: `docs/internals/mir-stack-maps.md`.

use std::collections::{BTreeSet, HashMap, HashSet};

use common::Instruction;

use crate::il::IlOp;

use super::func::MirFunc;
use super::inst::{BlockId, LocalId, MirInst, Terminator, ValueId};
use super::ty::MirTy;

/// Live heap words at one Alloc or GcBarrier dest (S2a).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveRootSet {
    /// Dest of the `Alloc` or `GcBarrier` this set belongs to.
    pub at: ValueId,
    /// Live heap-word SSA values (sorted). Includes the new object.
    pub values: Vec<ValueId>,
    /// IL slots that held a live heap word at this edge (sorted).
    pub slots: Vec<LocalId>,
}

/// Residual object-init opcodes (cold `IlOp::Byte`).
pub fn is_alloc_inst(inst: Instruction) -> bool {
    matches!(
        inst,
        Instruction::InitTyped | Instruction::INIT | Instruction::DenseMake
    )
}

/// IL that is an alloc / GC safepoint (I5 / Q9 R3). Unmapped bodies still refuse.
pub fn refuses_alloc(op: &IlOp) -> bool {
    refuse_reason(op).is_some()
}

/// Inventory label: `heap/aggregate` for Make*, `heap/alloc` for InitTyped.
pub fn refuse_reason(op: &IlOp) -> Option<&'static str> {
    match op {
        IlOp::MakeArray { .. } | IlOp::MakeTuple { .. } | IlOp::MakeEnum { .. } => {
            Some("heap/aggregate")
        }
        IlOp::Byte { byte, .. }
            if is_alloc_inst(*byte.bytecode())
                || *byte.bytecode() == Instruction::DenseMake =>
        {
            Some("heap/alloc")
        }
        IlOp::Byte { byte, .. }
            if matches!(
                *byte.bytecode(),
                Instruction::ArrayPush | Instruction::DenseArrayPush
            ) =>
        {
            Some("heap/grow")
        }
        IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::STRINGIFY => {
            Some("heap/format")
        }
        IlOp::Byte { byte, .. }
            if *byte.bytecode() == Instruction::FORMAT && byte.operand_u32() > 0 =>
        {
            Some("heap/format")
        }
        _ => None,
    }
}

/// Fill [`MirFunc::gc_roots`] and each `GcBarrier.roots` from SSA liveness.
///
/// Roots are heap words live *after* the edge, the new `Alloc` dest, and
/// heap words sitting in snapshotted IL slots. `GcBarrier` dest tokens
/// are not roots. Barrier `roots` are metadata, not SSA uses.
pub fn fill_live_roots(func: &mut MirFunc) {
    func.gc_roots.clear();
    if !func.has_gc_edge() {
        return;
    }
    let live_after = liveness_after(func);
    let barrier_tokens = barrier_dests(func);
    for (bi, block) in func.blocks.iter().enumerate() {
        let mut prev_alloc: Option<ValueId> = None;
        for (ii, inst) in block.insts.iter().enumerate() {
            match inst {
                MirInst::Alloc { dest, .. }
                | MirInst::ArrayPush { dest, .. }
                | MirInst::Format { dest, .. }
                | MirInst::Stringify { dest, .. } => {
                    let mut roots = live_heap(func, &live_after[bi][ii], &barrier_tokens);
                    roots.insert(*dest);
                    roots.append(&mut slot_heap(func, *dest));
                    let set = finish_set(func, *dest, roots);
                    func.gc_roots.push(set);
                    prev_alloc = Some(*dest);
                }
                MirInst::GcBarrier { dest, .. } => {
                    let mut roots = live_heap(func, &live_after[bi][ii], &barrier_tokens);
                    if let Some(obj) = prev_alloc {
                        roots.insert(obj);
                    }
                    roots.append(&mut slot_heap(func, *dest));
                    let set = finish_set(func, *dest, roots);
                    func.gc_roots.push(set);
                    prev_alloc = None;
                }
                _ => prev_alloc = None,
            }
        }
    }
    apply_barrier_roots(func);
}

fn finish_set(func: &MirFunc, at: ValueId, roots: BTreeSet<ValueId>) -> LiveRootSet {
    let values: Vec<ValueId> = roots.iter().copied().collect();
    let mut slots = BTreeSet::new();
    if let Some(env) = func.slot_env.get(&at) {
        for (slot, v) in env {
            if roots.contains(v) {
                slots.insert(*slot);
            }
        }
    }
    LiveRootSet {
        at,
        values,
        slots: slots.into_iter().collect(),
    }
}

fn apply_barrier_roots(func: &mut MirFunc) {
    let by_at: HashMap<ValueId, Vec<ValueId>> = func
        .gc_roots
        .iter()
        .map(|s| (s.at, s.values.clone()))
        .collect();
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            if let MirInst::GcBarrier { dest, roots, .. } = inst {
                if let Some(v) = by_at.get(dest) {
                    *roots = v.clone();
                }
            }
        }
    }
}

fn slot_heap(func: &MirFunc, at: ValueId) -> BTreeSet<ValueId> {
    let defined = defined_ssa(func);
    let mut roots = BTreeSet::new();
    if let Some(env) = func.slot_env.get(&at) {
        for (_, v) in env {
            if defined.contains(v) && func.ty(*v).is_heap_word() {
                roots.insert(*v);
            }
        }
    }
    roots
}

fn defined_ssa(func: &MirFunc) -> HashSet<ValueId> {
    let mut s = HashSet::new();
    for &p in &func.params {
        s.insert(p);
    }
    for b in &func.blocks {
        for inst in &b.insts {
            for d in inst.dests() {
                s.insert(d);
            }
        }
    }
    s
}

fn live_heap(
    func: &MirFunc,
    live: &HashSet<ValueId>,
    skip: &HashSet<ValueId>,
) -> BTreeSet<ValueId> {
    live.iter()
        .copied()
        .filter(|v| !skip.contains(v) && func.ty(*v).is_heap_word())
        .collect()
}

fn barrier_dests(func: &MirFunc) -> HashSet<ValueId> {
    let mut s = HashSet::new();
    for b in &func.blocks {
        for i in &b.insts {
            if let MirInst::GcBarrier { dest, .. } = i {
                s.insert(*dest);
            }
        }
    }
    s
}

/// `live_after[block][inst]` = SSA values live immediately after that inst.
fn liveness_after(func: &MirFunc) -> Vec<Vec<HashSet<ValueId>>> {
    let n = func.blocks.len();
    let mut live_in: Vec<HashSet<ValueId>> = vec![HashSet::new(); n];
    let mut after: Vec<Vec<HashSet<ValueId>>> = func
        .blocks
        .iter()
        .map(|b| vec![HashSet::new(); b.insts.len()])
        .collect();
    if n == 0 {
        return after;
    }
    let mut changed = true;
    while changed {
        changed = false;
        for bi in (0..n).rev() {
            let mut live = live_out_of(func, BlockId(bi as u32), &live_in);
            if let Some(term) = &func.blocks[bi].term {
                for u in term_uses(term) {
                    live.insert(u);
                }
            }
            let insts = &func.blocks[bi].insts;
            for ii in (0..insts.len()).rev() {
                if after[bi][ii] != live {
                    after[bi][ii] = live.clone();
                    changed = true;
                }
                let inst = &insts[ii];
                live.remove(&inst.dest());
                if !inst.is_phi() {
                    for u in data_uses(inst) {
                        live.insert(u);
                    }
                }
            }
            if live_in[bi] != live {
                live_in[bi] = live;
                changed = true;
            }
        }
    }
    after
}

fn live_out_of(func: &MirFunc, b: BlockId, live_in: &[HashSet<ValueId>]) -> HashSet<ValueId> {
    let mut live = HashSet::new();
    let Some(term) = &func.block(b).term else {
        return live;
    };
    for s in term.succs() {
        if s.index() >= live_in.len() {
            continue;
        }
        let phi_dests = phi_dests(func.block(s));
        for v in &live_in[s.index()] {
            if !phi_dests.contains(v) {
                live.insert(*v);
            }
        }
        for inst in &func.block(s).insts {
            let MirInst::Phi { args, .. } = inst else {
                break;
            };
            for (pred, v) in args {
                if *pred == b {
                    live.insert(*v);
                }
            }
        }
    }
    live
}

fn phi_dests(block: &super::func::MirBlock) -> HashSet<ValueId> {
    block
        .insts
        .iter()
        .take_while(|i| i.is_phi())
        .map(|i| i.dest())
        .collect()
}

fn data_uses(inst: &MirInst) -> Vec<ValueId> {
    match inst {
        MirInst::GcBarrier { .. } | MirInst::Deopt { .. } | MirInst::Const { .. } => Vec::new(),
        other => other.operands(),
    }
}

fn term_uses(term: &Terminator) -> Vec<ValueId> {
    match term {
        Terminator::Br { cond, .. } => vec![*cond],
        Terminator::JumpIfMatch { scrutinee, .. } => vec![*scrutinee],
        Terminator::Return { lo, hi } => lo.iter().copied().chain(hi.iter().copied()).collect(),
        Terminator::Jump { .. } | Terminator::Unreachable => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::{IlOp, Label};
    use crate::mir::builder::MirBuilder;
    use crate::mir::inst::{MirAllocKind, MirCmpOp, MirConst, MirGcKind};
    use crate::mir::lower::{try_lower_numeric, LowerHints};
    use common::{Byte, DebugLoc};

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    fn alloc_and_barrier(f: &MirFunc) -> (ValueId, ValueId, Vec<ValueId>) {
        let mut obj = None;
        let mut barrier = None;
        let mut roots = Vec::new();
        for b in &f.blocks {
            for i in &b.insts {
                match i {
                    MirInst::Alloc { dest, .. } if obj.is_none() => obj = Some(*dest),
                    MirInst::GcBarrier { dest, roots: r, .. } if barrier.is_none() => {
                        barrier = Some(*dest);
                        roots = r.clone();
                    }
                    _ => {}
                }
            }
        }
        (obj.expect("alloc"), barrier.expect("barrier"), roots)
    }

    #[test]
    fn make_and_init_are_alloc_barriers() {
        let loc = loc();
        assert_eq!(
            refuse_reason(&IlOp::MakeArray { arity: 1, loc }),
            Some("heap/aggregate")
        );
        assert_eq!(
            refuse_reason(&IlOp::MakeTuple { arity: 2, loc }),
            Some("heap/aggregate")
        );
        assert_eq!(
            refuse_reason(&IlOp::MakeEnum {
                tag: 0,
                arity: 1,
                loc,
            }),
            Some("heap/aggregate")
        );
        assert_eq!(
            refuse_reason(&IlOp::Byte {
                byte: Byte::new(Instruction::InitTyped).with_operand_u32(1),
                loc,
            }),
            Some("heap/alloc")
        );
        assert!(!refuses_alloc(&IlOp::Const { imm: 1, loc }));
        assert_eq!(
            refuse_reason(&IlOp::byte(Byte::new(Instruction::ArrayPush))),
            Some("heap/grow")
        );
        assert_eq!(
            refuse_reason(&IlOp::byte(Byte::new(Instruction::DenseArrayPush))),
            Some("heap/grow")
        );
        assert_eq!(
            refuse_reason(&IlOp::byte(
                Byte::new(Instruction::FORMAT).with_operand_u32(1)
            )),
            Some("heap/format")
        );
        assert!(!refuses_alloc(&IlOp::byte(
            Byte::new(Instruction::FORMAT).with_operand_u32(0)
        )));
        assert_eq!(
            refuse_reason(&IlOp::byte(Byte::new(Instruction::STRINGIFY))),
            Some("heap/format")
        );
    }

    #[test]
    fn roots_include_new_object_when_returned() {
        let mut b = MirBuilder::new("mk");
        let n = b.ins_const(MirConst::I64(1)).unwrap();
        let a = b.ins_alloc(MirAllocKind::Array, vec![n]).unwrap();
        let g = b.ins_gc_barrier(MirGcKind::Safepoint, vec![a]).unwrap();
        b.ret(Some(g)).unwrap();
        let f = b.finish().unwrap();
        f.verify().unwrap();
        let (obj, _g, roots) = alloc_and_barrier(&f);
        assert!(roots.contains(&obj), "new object in roots: {roots:?}");
        assert_eq!(roots.len(), 1);
        let side = f.live_roots_at(obj).expect("alloc sidecar");
        assert!(side.values.contains(&obj));
    }

    #[test]
    fn roots_include_live_heap_param_across_barrier() {
        let mut b = MirBuilder::new("keep");
        let xs = b.add_param(MirTy::HeapRef).unwrap();
        b.def_local(LocalId(0), xs).unwrap();
        let n = b.ins_const(MirConst::I64(1)).unwrap();
        let a = b.ins_alloc(MirAllocKind::Array, vec![n]).unwrap();
        let g = b.ins_gc_barrier(MirGcKind::Safepoint, vec![a]).unwrap();
        let eq = b.ins_cmp(MirCmpOp::Eq, xs, a).unwrap();
        let _ = eq;
        b.ret(Some(g)).unwrap();
        let f = b.finish().unwrap();
        f.verify().unwrap();
        let (obj, _g, roots) = alloc_and_barrier(&f);
        assert!(roots.contains(&obj), "{roots:?}");
        assert!(roots.contains(&xs), "live heap param: {roots:?}");
        let side = f.live_roots_at(_g).expect("barrier sidecar");
        assert!(side.values.contains(&xs) && side.values.contains(&obj));
        assert!(
            side.slots.iter().any(|s| s.0 == 0),
            "slot 0: {:?}",
            side.slots
        );
    }

    #[test]
    fn dead_heap_param_is_not_a_root() {
        let mut b = MirBuilder::new("dead");
        let unused = b.add_param(MirTy::HeapRef).unwrap();
        let n = b.ins_const(MirConst::I64(1)).unwrap();
        let a = b.ins_alloc(MirAllocKind::Array, vec![n]).unwrap();
        let g = b.ins_gc_barrier(MirGcKind::Safepoint, vec![a]).unwrap();
        b.ret(Some(g)).unwrap();
        let f = b.finish().unwrap();
        let (_obj, _g, roots) = alloc_and_barrier(&f);
        assert!(!roots.contains(&unused), "dead param in roots: {roots:?}");
    }

    #[test]
    fn second_barrier_keeps_prior_live_object() {
        let mut b = MirBuilder::new("two");
        let n1 = b.ins_const(MirConst::I64(1)).unwrap();
        let a = b.ins_alloc(MirAllocKind::Array, vec![n1]).unwrap();
        let _g1 = b.ins_gc_barrier(MirGcKind::Safepoint, vec![a]).unwrap();
        let n2 = b.ins_const(MirConst::I64(2)).unwrap();
        let c = b.ins_alloc(MirAllocKind::Array, vec![n2]).unwrap();
        let g2 = b.ins_gc_barrier(MirGcKind::Safepoint, vec![c]).unwrap();
        let _eq = b.ins_cmp(MirCmpOp::Eq, a, c).unwrap();
        b.ret(Some(g2)).unwrap();
        let f = b.finish().unwrap();
        f.verify().unwrap();
        let mut barriers = f.blocks.iter().flat_map(|bl| {
            bl.insts.iter().filter_map(|i| match i {
                MirInst::GcBarrier { dest, roots, .. } => Some((*dest, roots.clone())),
                _ => None,
            })
        });
        let (_g1, r1) = barriers.next().expect("g1");
        let (_g2, r2) = barriers.next().expect("g2");
        assert!(r1.contains(&a) && !r1.contains(&c), "first: {r1:?}");
        assert!(r2.contains(&a) && r2.contains(&c), "second: {r2:?}");
    }

    #[test]
    fn il_lower_roots_new_object_and_live_slot() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Const { imm: 1, loc },
            IlOp::MakeArray { arity: 1, loc },
            IlOp::StorePop { slot: 1, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Bin {
                op: common::Instruction::EQ,
                loc,
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let mut hints = LowerHints::new("keep_slot");
        hints.allow_alloc = true;
        hints.param_count = 1;
        hints.slot_ty.insert(0, MirTy::HeapRef);
        hints.slot_ty.insert(1, MirTy::HeapRef);
        let f = try_lower_numeric(&ops, &hints).expect("lower");
        f.verify().unwrap();
        let xs = f.params[0];
        let (obj, g, roots) = alloc_and_barrier(&f);
        assert!(roots.contains(&obj), "new object: {roots:?}");
        assert!(roots.contains(&xs), "live slot/param: {roots:?}");
        let side = f.live_roots_at(g).expect("barrier sidecar");
        assert!(
            side.slots.iter().any(|s| s.0 == 0),
            "IL slot 0: {:?}",
            side.slots
        );
    }
}
