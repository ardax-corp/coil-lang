//! Scalar replacement of objects that only meet at a φ.
//!
//! `let s = pat.find_in(hay, from) ?? new Span(-1, -1)` makes one object
//! on either path, `Some(new Span(at, end))` from the inlined `find_in`
//! or the default, then reads `s.start` / `s.end` once the paths meet. No
//! object needs to exist: each read field becomes a φ of the values the
//! paths stored, and both allocations go.
//!
//! Two rewrites get there:
//!
//! 1. A block whose only work is testing a niche `Option` φ for `None`
//!    (`lnot`), reached from one predecessor that brings `None` and one
//!    that brings a fresh object, is skipped: each predecessor jumps
//!    straight to the arm its value takes, and the arm reads the
//!    predecessor's value in place of the φ.
//! 2. A `HeapRef` φ whose every incoming value is a fresh `alloc.object`
//!    (seen through `Word` casts to and from the niche word), used for
//!    nothing but `heapsetfield` in its own block, and whose own uses are
//!    all `heaploadfield`, becomes one φ per read field.
//!
//! An object that escapes or is stored to after the φ keeps its
//! allocation. The passes run before the deopt sites are drawn from the
//! slot environments, which lose the values that go.

use std::collections::{HashMap, HashSet};

use super::cse::dce;
use super::func::MirFunc;
use super::inst::{BlockId, MirAllocKind, MirCastKind, MirConst, MirInst, MirUnaryOp, Terminator, ValueId};
use super::ty::MirTy;

/// Run both rewrites. Returns how many φs or tests they removed.
pub fn phi_sroa(func: &mut MirFunc) -> usize {
    // A debugger reads the locals where the source keeps them.
    if func.debug_keep {
        return 0;
    }
    let threaded = thread_niche_tests(func);
    if threaded > 0 {
        forward_word_round_trips(func);
        dce(func);
    }
    let split = split_object_phis(func);
    if split > 0 {
        dce(func);
    }
    threaded + split
}

/// Where each value is defined: its block and the instruction.
fn defs(func: &MirFunc) -> HashMap<ValueId, (BlockId, &MirInst)> {
    func.blocks
        .iter()
        .flat_map(|b| b.insts.iter().map(move |i| (i.dest(), (b.id, i))))
        .collect()
}

/// Drop removed values from the sidecars that name them.
fn forget(func: &mut MirFunc, gone: &HashSet<ValueId>) {
    for inst in func.blocks.iter_mut().flat_map(|b| &mut b.insts) {
        if let MirInst::GcBarrier { roots, .. } = inst {
            roots.retain(|v| !gone.contains(v));
        }
    }
    func.slot_env.retain(|at, _| !gone.contains(at));
    for env in func.slot_env.values_mut() {
        env.retain(|(_, v)| !gone.contains(v));
    }
    func.debug_slots.retain(|_, v| !gone.contains(v));
    func.gc_roots.retain(|s| !gone.contains(&s.at));
    for set in &mut func.gc_roots {
        set.values.retain(|v| !gone.contains(v));
    }
}

/// Every block that reads `v` (in an instruction or its terminator).
fn user_blocks(func: &MirFunc, v: ValueId) -> HashSet<BlockId> {
    let mut out = HashSet::new();
    for b in &func.blocks {
        let in_insts = b.insts.iter().any(|i| i.operands().contains(&v));
        let in_term = b.term.as_ref().is_some_and(|t| term_reads(t, v));
        if in_insts || in_term {
            out.insert(b.id);
        }
    }
    out
}

fn term_reads(term: &Terminator, v: ValueId) -> bool {
    let mut hit = false;
    term.clone().rewrite_values(|x| {
        hit |= x == v;
        x
    });
    hit
}

/// What a niche `Option` word is known to hold: `Some(true)` for `None`
/// (the zero word), `Some(false)` for a fresh object.
fn niche_is_none(defs: &HashMap<ValueId, (BlockId, &MirInst)>, v: ValueId) -> Option<bool> {
    match defs.get(&v)?.1 {
        MirInst::Cast {
            kind: MirCastKind::Word,
            to: MirTy::NicheOpt,
            src,
            ..
        } => match defs.get(src)?.1 {
            MirInst::Const { c: MirConst::I64(0), .. } => Some(true),
            MirInst::Alloc { .. } => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Rewrite 1: skip a block that only tests a niche φ whose value each
/// predecessor knows.
fn thread_niche_tests(func: &mut MirFunc) -> usize {
    let mut hits = 0;
    while thread_one(func) {
        hits += 1;
    }
    if hits > 0 {
        let reached = reachable(func);
        let order: Vec<BlockId> = func.blocks.iter().map(|b| b.id).filter(|b| reached.contains(b)).collect();
        super::from_hir::tidy(func, &order);
    }
    hits
}

/// Thread the first niche test that fits; `false` when none does.
fn thread_one(func: &mut MirFunc) -> bool {
    let preds = func.preds();
    for bi in 0..func.blocks.len() {
        let b = func.blocks[bi].id;
        let Some(Terminator::Br { cond, taken, not_taken }) = func.blocks[bi].term else {
            continue;
        };
        let insts = &func.blocks[bi].insts;
        let Some((MirInst::Unary { dest: test, op: MirUnaryOp::Not, src: opt }, phis)) = insts.split_last() else {
            continue;
        };
        if *test != cond || !phis.iter().all(MirInst::is_phi) || func.ty(*opt) != MirTy::NicheOpt {
            continue;
        }
        let (test, opt) = (*test, *opt);
        let phis: Vec<MirInst> = phis.to_vec();
        let from = &preds[b.index()];
        if from.len() != 2
            || taken == not_taken
            || taken == b
            || not_taken == b
            || preds[taken.index()] != [b]
            || preds[not_taken.index()] != [b]
            || from.iter().any(|p| !matches!(func.block(*p).term, Some(Terminator::Jump { dest }) if dest == b))
        {
            continue;
        }
        let incoming = |phi: &MirInst, p: BlockId| match phi {
            MirInst::Phi { args, .. } => args.iter().find(|(f, _)| *f == p).map(|&(_, v)| v),
            _ => None,
        };
        let Some(opt_phi) = phis.iter().find(|i| i.dest() == opt) else {
            continue;
        };
        let d = defs(func);
        let known: Option<Vec<bool>> = from
            .iter()
            .map(|&p| incoming(opt_phi, p).and_then(|v| niche_is_none(&d, v)))
            .collect();
        let Some(known) = known else {
            continue;
        };
        // One predecessor brings `None` (the test holds: `taken`), the
        // other an object.
        let (none_from, some_from) = match known.as_slice() {
            [true, false] => (from[0], from[1]),
            [false, true] => (from[1], from[0]),
            _ => continue,
        };
        // The φs and the test are read only here and in the two arms.
        let mut local = true;
        for v in phis.iter().map(MirInst::dest).chain([test]) {
            let users = user_blocks(func, v);
            if users.iter().any(|u| ![b, taken, not_taken].contains(u)) {
                local = false;
            }
        }
        if !local || user_blocks(func, test) != HashSet::from([b]) {
            continue;
        }
        let maps: Vec<(BlockId, BlockId, HashMap<ValueId, ValueId>)> = [(none_from, taken), (some_from, not_taken)]
            .into_iter()
            .map(|(p, arm)| {
                let map = phis.iter().filter_map(|phi| Some((phi.dest(), incoming(phi, p)?))).collect();
                (p, arm, map)
            })
            .collect();
        if maps.iter().any(|(_, _, m)| m.len() != phis.len()) {
            continue;
        }
        for (p, arm, map) in maps {
            func.block_mut(p).term = Some(Terminator::Jump { dest: arm });
            let block = func.block_mut(arm);
            for inst in &mut block.insts {
                inst.rewrite_values(|v| map.get(&v).copied().unwrap_or(v));
                if let MirInst::Phi { args, .. } = inst {
                    for (f, _) in args.iter_mut() {
                        if *f == b {
                            *f = p;
                        }
                    }
                }
            }
            if let Some(term) = &mut block.term {
                term.rewrite_values(|v| map.get(&v).copied().unwrap_or(v));
            }
        }
        let gone: HashSet<ValueId> = phis.iter().map(MirInst::dest).chain([test]).collect();
        forget(func, &gone);
        let block = &mut func.blocks[bi];
        block.insts.clear();
        block.term = Some(Terminator::Unreachable);
        func.term_locs.remove(&b);
        return true;
    }
    false
}

fn reachable(func: &MirFunc) -> HashSet<BlockId> {
    let mut seen = HashSet::from([func.entry]);
    let mut work = vec![func.entry];
    while let Some(b) = work.pop() {
        for s in func.block(b).term.as_ref().map(Terminator::succs).unwrap_or_default() {
            if seen.insert(s) {
                work.push(s);
            }
        }
    }
    seen
}

/// `Word` casts heap → niche → heap read the heap word itself.
fn forward_word_round_trips(func: &mut MirFunc) {
    let d = defs(func);
    let mut subst: HashMap<ValueId, ValueId> = HashMap::new();
    for (&v, (_, inst)) in &d {
        if let MirInst::Cast { kind: MirCastKind::Word, to: MirTy::HeapRef, src, .. } = inst
            && let Some((_, MirInst::Cast { kind: MirCastKind::Word, to: MirTy::NicheOpt, src: heap, .. })) = d.get(src)
            && func.ty(*heap) == MirTy::HeapRef
        {
            subst.insert(v, *heap);
        }
    }
    if subst.is_empty() {
        return;
    }
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            inst.rewrite_values(|v| subst.get(&v).copied().unwrap_or(v));
        }
        if let Some(term) = &mut block.term {
            term.rewrite_values(|v| subst.get(&v).copied().unwrap_or(v));
        }
    }
    func.rewrite_debug_slots(|v| subst.get(&v).copied().unwrap_or(v));
}

/// One fresh object reaching a φ: its allocation and the value each field
/// holds on the way out of its block.
struct Fresh {
    alloc: ValueId,
    fields: HashMap<u32, ValueId>,
}

/// The fields `alloc` holds when its block ends, when it is an object
/// whose only uses are stores into it in that block (and `phi`).
fn fresh_object(func: &MirFunc, alloc: ValueId, phi: ValueId) -> Option<Fresh> {
    let (block, nfields, elems) = func.blocks.iter().find_map(|b| {
        b.insts.iter().find_map(|i| match i {
            MirInst::Alloc { dest, kind: MirAllocKind::Object { nfields, .. }, elems, .. } if *dest == alloc => {
                Some((b.id, *nfields, elems.clone()))
            }
            _ => None,
        })
    })?;
    if !elems.is_empty() && elems.len() != nfields as usize {
        return None;
    }
    let mut fields: HashMap<u32, ValueId> = elems.iter().enumerate().map(|(i, &v)| (i as u32, v)).collect();
    let mut seen_alloc = false;
    for b in &func.blocks {
        for inst in &b.insts {
            if inst.dest() == alloc {
                seen_alloc = true;
                continue;
            }
            match inst {
                MirInst::HeapFieldStore { dest, object, value, index } if *object == alloc => {
                    if b.id != block || !seen_alloc || *value == alloc {
                        return None;
                    }
                    // The store's own value (the VM leaves it on the stack)
                    // must be unused: it goes with the object.
                    if !user_blocks(func, *dest).is_empty() {
                        return None;
                    }
                    fields.insert(*index, *value);
                }
                MirInst::Phi { dest, .. } if *dest == phi => {}
                // A safepoint's root list only says the word is live.
                MirInst::GcBarrier { .. } => {}
                _ if inst.operands().contains(&alloc) => return None,
                _ => {}
            }
        }
        if b.term.as_ref().is_some_and(|t| term_reads(t, alloc)) {
            return None;
        }
    }
    Some(Fresh { alloc, fields })
}

/// Rewrite 2: a φ of fresh objects read only field by field becomes a φ
/// per read field.
fn split_object_phis(func: &mut MirFunc) -> usize {
    let mut hits = 0;
    let candidates: Vec<(BlockId, ValueId, Vec<(BlockId, ValueId)>)> = func
        .blocks
        .iter()
        .flat_map(|b| {
            b.insts.iter().filter_map(move |i| match i {
                MirInst::Phi { dest, ty: MirTy::HeapRef, args } => Some((b.id, *dest, args.clone())),
                _ => None,
            })
        })
        .collect();
    let debug: HashSet<ValueId> = func.debug_slots.values().copied().collect();
    for (block, phi, args) in candidates {
        if func.debug_keep && debug.contains(&phi) {
            continue;
        }
        // Every use of the φ is a field load; an incoming object must not
        // be the φ itself (a loop-carried object).
        let mut loads: Vec<(ValueId, u32)> = Vec::new();
        let mut ok = true;
        for b in &func.blocks {
            for inst in &b.insts {
                match inst {
                    MirInst::HeapFieldLoad { dest, object, index } if *object == phi => loads.push((*dest, *index)),
                    MirInst::Phi { dest, .. } if *dest == phi => {}
                    MirInst::GcBarrier { .. } => {}
                    _ if inst.operands().contains(&phi) => ok = false,
                    _ => {}
                }
            }
            if b.term.as_ref().is_some_and(|t| term_reads(t, phi)) {
                ok = false;
            }
        }
        if !ok || loads.is_empty() {
            continue;
        }
        let fresh: Option<Vec<(BlockId, Fresh)>> =
            args.iter().map(|&(p, v)| Some((p, fresh_object(func, v, phi)?))).collect();
        let Some(fresh) = fresh else {
            continue;
        };
        // An object reaching the φ twice (or another φ) stays an object.
        let allocs: HashSet<ValueId> = fresh.iter().map(|(_, f)| f.alloc).collect();
        if allocs.len() != fresh.len() || func.blocks.iter().flat_map(|b| &b.insts).any(|i| {
            matches!(i, MirInst::Phi { dest, args, .. } if *dest != phi && args.iter().any(|(_, v)| allocs.contains(v)))
        }) {
            continue;
        }
        // Each read field: every path stored it, all as one word type,
        // the load's own.
        let indices: Vec<u32> = {
            let mut v: Vec<u32> = loads.iter().map(|&(_, i)| i).collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        let mut field_phis: HashMap<u32, (ValueId, Vec<(BlockId, ValueId)>)> = HashMap::new();
        for &index in &indices {
            let ty = func.ty(loads.iter().find(|&&(_, i)| i == index).unwrap().0);
            let incoming: Option<Vec<(BlockId, ValueId)>> = fresh
                .iter()
                .map(|(p, f)| f.fields.get(&index).filter(|v| func.ty(**v) == ty).map(|&v| (*p, v)))
                .collect();
            let Some(incoming) = incoming else {
                ok = false;
                break;
            };
            if loads.iter().any(|&(d, i)| i == index && func.ty(d) != ty) {
                ok = false;
                break;
            }
            let dest = ValueId(func.types.len() as u32);
            func.types.push(ty);
            field_phis.insert(index, (dest, incoming));
        }
        if !ok {
            continue;
        }
        let subst: HashMap<ValueId, ValueId> = loads.iter().map(|&(d, i)| (d, field_phis[&i].0)).collect();
        let dead_stores: HashSet<ValueId> = func
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .filter_map(|i| match i {
                MirInst::HeapFieldStore { dest, object, .. } if allocs.contains(object) => Some(*dest),
                _ => None,
            })
            .collect();
        for b in &mut func.blocks {
            b.insts.retain(|i| {
                let d = i.dest();
                d != phi && !allocs.contains(&d) && !dead_stores.contains(&d) && !subst.contains_key(&d)
            });
            for inst in &mut b.insts {
                inst.rewrite_values(|v| subst.get(&v).copied().unwrap_or(v));
            }
            if let Some(term) = &mut b.term {
                term.rewrite_values(|v| subst.get(&v).copied().unwrap_or(v));
            }
        }
        let mut new_phis: Vec<(u32, MirInst)> = field_phis
            .into_iter()
            .map(|(i, (dest, args))| (i, MirInst::Phi { dest, ty: func.ty(dest), args }))
            .collect();
        new_phis.sort_by_key(|(i, _)| *i);
        let at = func.block_mut(block);
        for (k, (_, inst)) in new_phis.into_iter().enumerate() {
            at.insts.insert(k, inst);
        }
        func.rewrite_debug_slots(|v| subst.get(&v).copied().unwrap_or(v));
        for env in func.slot_env.values_mut() {
            for (_, v) in env.iter_mut() {
                *v = subst.get(v).copied().unwrap_or(*v);
            }
        }
        let gone: HashSet<ValueId> = allocs.iter().copied().chain([phi]).chain(dead_stores).collect();
        forget(func, &gone);
        hits += 1;
    }
    hits
}
