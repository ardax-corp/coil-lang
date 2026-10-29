//! Scalar promotion: keep an object's field in a register across a loop
//! (locality step 4, `docs/internals/locality.md`).
//!
//! A loop that writes `o.f`, with `o` made before the loop and no other
//! object touching that slot, reads `o.f` once before the loop, carries it
//! in a φ, and stores it once on each exit. The source stored it at each
//! assignment, so a panic inside the loop must leave the field as those
//! stores did: before each instruction that may trap, the pass places a
//! [`MirInst::PanicStore`] naming the word the field holds there, and
//! dense emission gives that stretch of code a pad that stores it before
//! the unwind goes on ([`super::emit::PanicPads`]).
//!
//! The loop may hold only code that cannot see the slot otherwise:
//! arithmetic, compares, casts, element reads and writes, field reads and
//! writes, explicit panics and pure natives. A call, an allocation, a
//! yield, IO or any other effect refuses the loop.

use std::collections::{HashMap, HashSet};

use super::effects::host_is_pure;
use super::func::MirFunc;
use super::inst::{BlockId, MirInst, Terminator, ValueId};
use super::licm::{natural_loops, LoopInfo};
use super::ty::MirTy;

/// Promote the fields loops write through one invariant object; the
/// number of fields promoted.
pub fn promote_fields(func: &mut MirFunc) -> usize {
    // A `defer` pad, a debugger or a coverage run reads the heap or the
    // source's code where it stands.
    if func.cleanup.is_some() || !func.homes.is_empty() || func.debug_keep || func.keep_lines {
        return 0;
    }
    let mut done = 0;
    // Innermost first. A promoted loop holds `PanicStore`s, which refuse
    // it (and every loop around it) the next time round.
    loop {
        let mut loops = natural_loops(func);
        loops.sort_by_key(|l| l.blocks.len());
        let Some(n) = loops.iter().map(|lp| promote_loop(func, lp)).find(|&n| n > 0) else {
            return done;
        };
        done += n;
    }
}

/// One promoted field: its object, slot and word type.
#[derive(Clone, Copy)]
struct Field {
    object: ValueId,
    index: u32,
    ty: MirTy,
}

fn promote_loop(func: &mut MirFunc, lp: &LoopInfo) -> usize {
    let preds = func.preds();
    let header = lp.header;
    let outside: Vec<BlockId> = preds[header.index()].iter().copied().filter(|p| !lp.blocks.contains(p)).collect();
    let [pre] = outside[..] else {
        return 0;
    };
    if !matches!(func.block(pre).term, Some(Terminator::Jump { dest }) if dest == header) {
        return 0;
    }
    // Each exit is a block only its loop block enters: the stores go there.
    let mut exits: Vec<(BlockId, BlockId)> = Vec::new();
    for &b in &lp.blocks {
        let Some(term) = &func.block(b).term else {
            return 0;
        };
        if !matches!(term, Terminator::Jump { .. } | Terminator::Br { .. }) {
            return 0;
        }
        for s in term.succs() {
            if lp.blocks.contains(&s) {
                continue;
            }
            if preds[s.index()].len() != 1 {
                return 0;
            }
            if !exits.contains(&(s, b)) {
                exits.push((s, b));
            }
        }
    }
    let fields = candidates(func, lp);
    if fields.is_empty() {
        return 0;
    }
    rewrite(func, lp, pre, &preds, &exits, &fields);
    fields.len()
}

/// The fields `lp` may keep in registers: a slot the loop writes, touched
/// only through one object made before the loop, holding a scalar.
fn candidates(func: &MirFunc, lp: &LoopInfo) -> Vec<Field> {
    let defined: HashSet<ValueId> = lp
        .blocks
        .iter()
        .flat_map(|&b| func.block(b).insts.iter().flat_map(MirInst::dests))
        .collect();
    let consts: HashSet<ValueId> = func
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter(|i| matches!(i, MirInst::Const { .. }))
        .map(MirInst::dest)
        .collect();
    // Per slot: the objects touching it, whether the loop writes it, and
    // the word types it moves.
    struct Slot {
        objects: HashSet<ValueId>,
        stored: bool,
        tys: HashSet<MirTy>,
        refused: bool,
    }
    let mut slots: HashMap<u32, Slot> = HashMap::new();
    let mut blocks: Vec<BlockId> = lp.blocks.iter().copied().collect();
    blocks.sort();
    for b in blocks {
        for inst in &func.block(b).insts {
            let (object, index, word, store) = match inst {
                MirInst::HeapFieldLoad { dest, object, index } => (*object, *index, *dest, false),
                MirInst::HeapFieldStore { object, value, index, .. } => (*object, *index, *value, true),
                _ if allowed(inst) => continue,
                _ => return Vec::new(),
            };
            let slot = slots.entry(index).or_insert_with(|| Slot {
                objects: HashSet::new(),
                stored: false,
                tys: HashSet::new(),
                refused: false,
            });
            slot.objects.insert(object);
            slot.stored |= store;
            slot.tys.insert(func.ty(word));
            // A constant word has no register for the pad to read.
            slot.refused |= store && consts.contains(&word);
        }
    }
    let mut out: Vec<Field> = slots
        .into_iter()
        .filter_map(|(index, s)| {
            let [object] = s.objects.iter().copied().collect::<Vec<_>>()[..] else {
                return None;
            };
            let [ty] = s.tys.iter().copied().collect::<Vec<_>>()[..] else {
                return None;
            };
            let ok = s.stored
                && !s.refused
                && !defined.contains(&object)
                && func.ty(object) == MirTy::HeapRef
                && matches!(ty, MirTy::I64 | MirTy::F64);
            ok.then_some(Field { object, index, ty })
        })
        .collect();
    out.sort_by_key(|f| (f.object, f.index));
    out
}

/// Code a promoted loop may hold besides its field ops: nothing that calls
/// out, allocates, suspends or touches the heap other than by element.
fn allowed(inst: &MirInst) -> bool {
    match inst {
        MirInst::Phi { .. }
        | MirInst::Const { .. }
        | MirInst::Bin { .. }
        | MirInst::Cmp { .. }
        | MirInst::Unary { .. }
        | MirInst::Cast { .. }
        | MirInst::ArrayLen { .. }
        | MirInst::Index { .. }
        | MirInst::StoreIndex { .. }
        | MirInst::KeepAlive { .. }
        | MirInst::ClearWord { .. }
        | MirInst::Panic { .. } => true,
        MirInst::HostInvoke { native_id, .. } => host_is_pure(*native_id),
        _ => false,
    }
}

/// May panic: the heap must hold the source's stores by then.
fn traps(func: &MirFunc, inst: &MirInst) -> bool {
    inst.may_trap(func)
        || matches!(
            inst,
            MirInst::StoreIndex { unchecked: false, .. } | MirInst::Cast { .. } | MirInst::HostInvoke { .. } | MirInst::Panic { .. }
        )
}

fn fresh(func: &mut MirFunc, ty: MirTy) -> ValueId {
    func.types.push(ty);
    ValueId(func.types.len() as u32 - 1)
}

/// Loop blocks from `header` in reverse postorder (back edges to the
/// header left out).
fn loop_order(func: &MirFunc, lp: &LoopInfo) -> Vec<BlockId> {
    let mut seen = HashSet::new();
    let mut post = Vec::new();
    let mut stack = vec![(lp.header, false)];
    while let Some((b, done)) = stack.pop() {
        if done {
            post.push(b);
            continue;
        }
        if !seen.insert(b) {
            continue;
        }
        stack.push((b, true));
        if let Some(term) = &func.block(b).term {
            for s in term.succs().into_iter().rev() {
                if lp.blocks.contains(&s) && s != lp.header && !seen.contains(&s) {
                    stack.push((s, false));
                }
            }
        }
    }
    post.reverse();
    post
}

fn rewrite(func: &mut MirFunc, lp: &LoopInfo, pre: BlockId, preds: &[Vec<BlockId>], exits: &[(BlockId, BlockId)], fields: &[Field]) {
    let is = |f: &Field, object: ValueId, index: u32| f.object == object && f.index == index;
    // Read each field once before the loop.
    let mut init = Vec::with_capacity(fields.len());
    for f in fields {
        let dest = fresh(func, f.ty);
        func.block_mut(pre).insts.push(MirInst::HeapFieldLoad { dest, object: f.object, index: f.index });
        init.push(dest);
    }
    // A φ per field where loop paths meet; elsewhere the one way in.
    let order = loop_order(func, lp);
    let mut phis: HashMap<BlockId, Vec<ValueId>> = HashMap::new();
    for &b in &order {
        if b == lp.header || preds[b.index()].len() > 1 {
            let vs = fields.iter().map(|f| fresh(func, f.ty)).collect();
            phis.insert(b, vs);
        }
    }
    let mut replace: HashMap<ValueId, ValueId> = HashMap::new();
    let mut out_of: HashMap<BlockId, Vec<ValueId>> = HashMap::new();
    let mut store_loc = None;
    for &b in &order {
        let mut cur = match phis.get(&b) {
            Some(vs) => vs.clone(),
            None => out_of[&preds[b.index()][0]].clone(),
        };
        let insts = std::mem::take(&mut func.block_mut(b).insts);
        let mut kept = Vec::with_capacity(insts.len());
        for inst in insts {
            match &inst {
                MirInst::HeapFieldLoad { dest, object, index } => {
                    if let Some(k) = fields.iter().position(|f| is(f, *object, *index)) {
                        replace.insert(*dest, cur[k]);
                        continue;
                    }
                }
                MirInst::HeapFieldStore { dest, object, value, index } => {
                    if let Some(k) = fields.iter().position(|f| is(f, *object, *index)) {
                        replace.insert(*dest, *value);
                        store_loc.get_or_insert(*dest);
                        cur[k] = *value;
                        continue;
                    }
                }
                _ if traps(func, &inst) => {
                    for (f, &value) in fields.iter().zip(&cur) {
                        let dest = fresh(func, MirTy::Bool);
                        kept.push(MirInst::PanicStore { dest, object: f.object, value, index: f.index });
                    }
                }
                _ => {}
            }
            kept.push(inst);
        }
        func.block_mut(b).insts = kept;
        out_of.insert(b, cur);
    }
    // Fill the φs: the header's from before the loop and its latches.
    for (&b, vs) in &phis {
        let mut new = Vec::with_capacity(vs.len());
        for (k, (&dest, f)) in vs.iter().zip(fields).enumerate() {
            let mut args: Vec<(BlockId, ValueId)> = Vec::new();
            for &p in &preds[b.index()] {
                let v = if p == pre && b == lp.header { init[k] } else { out_of[&p][k] };
                args.push((p, v));
            }
            new.push(MirInst::Phi { dest, ty: f.ty, args });
        }
        let insts = &mut func.block_mut(b).insts;
        insts.splice(0..0, new);
    }
    // Store each field on the way out, before anything else runs there.
    for &(exit, from) in exits {
        let at = func.block(exit).insts.iter().take_while(|i| i.is_phi()).count();
        let mut stores = Vec::with_capacity(fields.len());
        for (k, f) in fields.iter().enumerate() {
            let dest = fresh(func, f.ty);
            if let Some(loc) = store_loc {
                let l = func.loc_of(loc);
                func.value_locs.insert(dest, l);
            }
            stores.push(MirInst::HeapFieldStore { dest, object: f.object, value: out_of[&from][k], index: f.index });
        }
        func.block_mut(exit).insts.splice(at..at, stores);
    }
    // A φ whose ways in all carry one word is that word.
    let created: HashSet<ValueId> = phis.values().flatten().copied().collect();
    let resolve = |replace: &HashMap<ValueId, ValueId>, mut v: ValueId| {
        while let Some(&n) = replace.get(&v) {
            if n == v {
                break;
            }
            v = n;
        }
        v
    };
    loop {
        let mut more = false;
        for block in &func.blocks {
            for inst in &block.insts {
                let MirInst::Phi { dest, args, .. } = inst else {
                    continue;
                };
                if !created.contains(dest) || replace.contains_key(dest) {
                    continue;
                }
                let mut ins = args.iter().map(|&(_, v)| resolve(&replace, v)).filter(|v| v != dest);
                let Some(first) = ins.next() else {
                    continue;
                };
                if ins.all(|v| v == first) {
                    replace.insert(*dest, first);
                    more = true;
                }
            }
        }
        if !more {
            break;
        }
    }
    for block in &mut func.blocks {
        block.insts.retain(|i| !matches!(i, MirInst::Phi { dest, .. } if created.contains(dest) && replace.contains_key(dest)));
        for inst in &mut block.insts {
            inst.rewrite_values(|v| resolve(&replace, v));
        }
        if let Some(term) = &mut block.term {
            term.rewrite_values(|v| resolve(&replace, v));
        }
    }
    func.rewrite_debug_slots(|v| resolve(&replace, v));
}
