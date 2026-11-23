//! Reference count insertion (reference counts plan step 4): the
//! `RcInc` / `RcDec` ops that make a frame own the objects its words name.
//!
//! Each value that may hold an object (a heap word, or an `int` word the
//! collector roots: [`pointer_words`], [`opaque_words`], [`pair_words`]) is
//! a *root* or an *alias* of one. An alias is the same object under another
//! word: a cast, the untagging bit op of a niche word, a store's dest (the
//! stored word) or an in-place push's (the array). A root is *borrowed*
//! (a parameter: the caller keeps it alive across the call) or *owned*:
//!
//! - a call's result is owned already (a body returns its result owned);
//! - a φ is owned: each way in hands it a word it owns (moved, when that
//!   way's last use is the φ, or incremented on the edge);
//! - anything else that defines a root (an allocation, a load, a host
//!   call's result, a copy) is incremented right after it, as a load from
//!   a container that may be written while the word is in use must be.
//!
//! An owned root is decremented right after its last use (its own or an
//! alias's), or on the edge where it dies: a way out of the block that it
//! is not live on. A `return` hands its words to the caller: an owned root
//! that dies there moves, anything else is incremented first. A payload a
//! `JumpIfMatch` binds is a load on its taken edge.
//!
//! Elision so far: numbers (constants, arithmetic, φs of them) are never
//! counted, and an inc / dec pair of one word in a block cancels when
//! nothing between them can release an object ([`cancel_pairs`]). No
//! ownership moves into calls yet. What it leaves as
//! leaks, which the oracle reports but which free nothing early: a tail
//! call's arguments (decrementing them after the call would end the tail
//! call), a frame that unwinds through a panic, a coroutine abandoned
//! before it finishes.

use std::collections::{HashMap, HashSet};
use crate::bodies::Label;

use super::frame_words::{barrier_pairs, heap_word, opaque_words, pair_words, pointer_words, ty_of};
use super::func::{MirBlock, MirFunc, is_tail_call_inst};
use super::inst::{BlockId, MirBinOp, MirCastKind, MirInst, Terminator, ValueId};
use super::ty::MirTy;

/// Whether this compile inserts the ops: a run that keeps counts
/// (`COIL_RC_CHECK`, as `machine::rc_check_mode` reads it).
pub fn rc_insert_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("COIL_RC_CHECK").is_ok_and(|v| !v.is_empty() && v != "0"))
}

/// Whether an unwound frame releases what it owned ([`unwind_homes`]):
/// on unless `COIL_RC_UNWIND=0`.
fn unwind_homes_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("COIL_RC_UNWIND").map_or(true, |v| v != "0"))
}

/// Insert the count ops into a program's bodies, each `(entry, body)`, and
/// their cleanup pads. A call to a body that only ever returns a number
/// gives no word to count. Returns how many ops it inserted.
pub fn insert_rc_all<'a>(bodies: impl IntoIterator<Item = (Option<Label>, &'a mut MirFunc)>) -> usize {
    let mut bodies: Vec<(Option<Label>, &mut MirFunc)> = bodies.into_iter().collect();
    let numbers = number_returns(bodies.iter().map(|(l, f)| (*l, &**f)));
    bodies.iter_mut().map(|(l, f)| insert_rc_in(f, false, &numbers, *l)).sum()
}

/// The entries of the bodies whose every return is a number (by its
/// definition: a constant, arithmetic, a comparison, a call of another
/// such body, a φ of those), found optimistically: every candidate is
/// assumed until one of its returns says otherwise.
fn number_returns<'a>(bodies: impl Iterator<Item = (Option<Label>, &'a MirFunc)>) -> HashSet<Label> {
    let cands: Vec<(Label, &MirFunc)> = bodies
        .filter_map(|(l, f)| Some((l?, f)))
        .filter(|(_, f)| f.ret_ty == Some(MirTy::I64) && !super::frame_words::is_generator(f) && f.blocks.iter().any(|b| matches!(b.term, Some(Terminator::Return { .. }))))
        .collect();
    let mut set: HashSet<Label> = cands.iter().map(|(l, _)| *l).collect();
    loop {
        let before = set.len();
        for (l, f) in &cands {
            if !set.contains(l) {
                continue;
            }
            let nums = number_words(f, &set);
            let used = used_as_numbers(f, true);
            let all = f.blocks.iter().all(|b| match &b.term {
                Some(Terminator::Return { lo, hi }) => hi.is_none() && lo.is_none_or(|v| nums.contains(&v) || used.contains(&v) && computed_with(f, v)),
                _ => true,
            });
            if !all {
                set.remove(l);
            }
        }
        if set.len() == before {
            return set;
        }
    }
}

/// The words of `func` that are numbers by their definition, a call of a
/// body in `numbers` included (φs optimistically).
fn number_words(func: &MirFunc, numbers: &HashSet<Label>) -> HashSet<ValueId> {
    let insts = || func.blocks.iter().flat_map(|b| &b.insts);
    let mut out: HashSet<ValueId> = insts()
        .filter(|i| match i {
            MirInst::Const { .. } | MirInst::Cmp { .. } => true,
            MirInst::Bin { op, .. } => !matches!(op, MirBinOp::BitAnd | MirBinOp::BitOr | MirBinOp::Xor),
            MirInst::Call { target, dest_hi: None, .. } => numbers.contains(target),
            MirInst::Phi { .. } => true,
            _ => false,
        })
        .map(MirInst::dest)
        .collect();
    loop {
        let before = out.len();
        for inst in insts() {
            if let MirInst::Phi { dest, args, .. } = inst
                && out.contains(dest)
                && args.iter().any(|(_, a)| !out.contains(a))
            {
                out.remove(dest);
            }
        }
        if out.len() == before {
            return out;
        }
    }
}

/// The `int` words of `func` that are numbers by how they are used: a
/// load (a field, an element, a payload, a static), a host result or a φ
/// whose every use computes with it (an arithmetic op, a compare, a
/// branch, an element index, a φ of such words, or a return when the body
/// returns only numbers). Nothing keeps one or reads through it, so none
/// needs an owner. Found optimistically: every candidate is assumed until
/// a use says otherwise.
fn used_as_numbers(func: &MirFunc, returns_number: bool) -> HashSet<ValueId> {
    let insts = || func.blocks.iter().flat_map(|b| &b.insts);
    let mut set: HashSet<ValueId> = insts()
        .filter(|i| {
            matches!(
                i,
                MirInst::ProductLoad { .. }
                    | MirInst::Index { .. }
                    | MirInst::LaneIndex { .. }
                    | MirInst::LoadStatic { .. }
                    | MirInst::MatchPayload { .. }
                    | MirInst::HostInvoke { dest_hi: None, .. }
                    | MirInst::Phi { .. }
            )
        })
        .flat_map(MirInst::dests)
        .filter(|v| ty_of(func, *v) == MirTy::I64)
        .collect();
    loop {
        let before = set.len();
        for i in insts() {
            let ok = |v: ValueId| match i {
                MirInst::Bin { op, .. } => !matches!(op, MirBinOp::BitAnd | MirBinOp::BitOr | MirBinOp::Xor),
                MirInst::Cmp { .. } => true,
                MirInst::Index { array, .. } | MirInst::LaneIndex { array, .. } => *array != v,
                MirInst::StoreIndex { array, value, .. } => *array != v && *value != v,
                MirInst::Phi { dest, .. } => set.contains(dest),
                _ => false,
            };
            let bad: Vec<ValueId> = i.operands().into_iter().filter(|v| set.contains(v) && !ok(*v)).collect();
            for v in bad {
                set.remove(&v);
            }
        }
        for t in func.blocks.iter().filter_map(|b| b.term.as_ref()) {
            let keeps: Vec<ValueId> = match t {
                Terminator::Br { .. } => Vec::new(),
                Terminator::Return { hi: None, .. } if returns_number => Vec::new(),
                _ => t.uses().collect(),
            };
            for v in keeps {
                set.remove(&v);
            }
        }
        if set.len() == before {
            return set;
        }
    }
}

/// Whether `v` is an operand of arithmetic, an ordered compare or an
/// element index: what only a number is (a word that is only returned
/// may be an object of a type variable, which [`used_as_numbers`] alone
/// would take for one).
fn computed_with(func: &MirFunc, v: ValueId) -> bool {
    use super::inst::MirCmpOp;
    func.blocks.iter().flat_map(|b| &b.insts).any(|i| match i {
        MirInst::Bin { op, lhs, rhs, .. } => !matches!(op, MirBinOp::BitAnd | MirBinOp::BitOr | MirBinOp::Xor) && (*lhs == v || *rhs == v),
        MirInst::Cmp { op, lhs, rhs, .. } => !matches!(op, MirCmpOp::Eq | MirCmpOp::Ne) && (*lhs == v || *rhs == v),
        MirInst::Index { index, .. } | MirInst::LaneIndex { index, .. } | MirInst::StoreIndex { index, .. } => *index == v,
        _ => false,
    })
}

/// Whether an instruction may write a word into an object, or run code
/// that may: what ends a load's borrow of its holder ([`Pass::new`]).
fn may_write_words(inst: &MirInst) -> bool {
    match inst {
        MirInst::Const { .. }
        | MirInst::Bin { .. }
        | MirInst::Cmp { .. }
        | MirInst::Unary { .. }
        | MirInst::Cast { .. }
        | MirInst::Phi { .. }
        | MirInst::ProductLoad { .. }
        | MirInst::Index { .. }
        | MirInst::LaneIndex { .. }
        | MirInst::ArrayLen { .. }
        | MirInst::MatchPayload { .. }
        | MirInst::HomeLoad { .. }
        | MirInst::HomeStore { .. }
        | MirInst::ClearWord { .. }
        | MirInst::KeepAlive { .. }
        | MirInst::GcBarrier { .. } => false,
        MirInst::HostInvoke { native_id, .. } => {
            use common::EffectFlags as E;
            let e = super::effects::host_effects(*native_id);
            [E::HEAP_MUT, E::UNKNOWN, E::FFI, E::GC, E::THREAD, E::YIELD, E::SUSPEND, E::ATTACH_PARK].iter().any(|&b| e.contains(b))
        }
        _ => true,
    }
}

/// Whether instruction `inst` only reads through `v` or computes with it:
/// a use that keeps no copy of the word.
fn reads_only(inst: &MirInst, v: ValueId) -> bool {
    match inst {
        MirInst::ProductLoad { object, .. } => *object == v,
        MirInst::Index { array, index, .. } | MirInst::LaneIndex { array, index, .. } => *array == v && *index != v,
        MirInst::ArrayLen { .. } | MirInst::Cmp { .. } | MirInst::KeepAlive { .. } | MirInst::ClearWord { .. } | MirInst::GcBarrier { .. } => true,
        MirInst::MatchPayload { scrutinee, .. } => *scrutinee == v,
        MirInst::HostInvoke { .. } => !may_write_words(inst),
        _ => false,
    }
}

/// [`insert_rc_all`] of a body, or of a cleanup pad (`pad`: it runs while the
/// frame unwinds, so it keeps no homes of its own).
fn insert_rc_in(func: &mut MirFunc, pad: bool, numbers: &HashSet<Label>, own: Option<Label>) -> usize {
    let mut n = 0;
    if let Some(p) = func.cleanup.as_deref_mut() {
        n += insert_rc_in(p, true, numbers, None);
    }
    // Counts free what an early free would (`hir/early_free.rs`): its
    // frees go, or a count would be decremented on a freed object.
    for inst in func.blocks.iter_mut().flat_map(|b| b.insts.iter_mut()) {
        if let MirInst::FreeEarly { dest, .. } = *inst {
            *inst = MirInst::Const { dest, c: super::inst::MirConst::Bool(false) };
        }
    }
    let pass = Pass::new(func, numbers, own.is_some_and(|l| numbers.contains(&l)));
    let (ops, homes) = pass.run(func, !pad);
    n += ops;
    n -= cancel_pairs(func, &homes);
    n + unwind_homes(func, &homes)
}

/// The owned words a panic may unwind past, each kept in a home of its
/// own while the frame owns it: a store where it becomes owned, a clear
/// (`0`) before each decrement and where a φ takes it over. The cleanup
/// pad (the body's, or a new one) decrements what the homes hold, so an
/// unwound frame releases what it owned. Returns how many ops it added.
fn unwind_homes(func: &mut MirFunc, homes: &HashMap<ValueId, u16>) -> usize {
    if homes.is_empty() {
        return 0;
    }
    let mut added = 0;
    let mut next = func.types.len() as u32;
    let mut new_types = Vec::new();
    let mut fresh = |ty: MirTy, new_types: &mut Vec<MirTy>| {
        let v = ValueId(next);
        next += 1;
        new_types.push(ty);
        v
    };
    // The clears before each decrement.
    for block in &mut func.blocks {
        let old = std::mem::take(&mut block.insts);
        for inst in old {
            if let MirInst::RcDec { src, guard: None, .. } = inst
                && let Some(&h) = homes.get(&src)
            {
                let zero = fresh(MirTy::I64, &mut new_types);
                block.insts.push(MirInst::Const { dest: zero, c: super::inst::MirConst::I64(0) });
                block.insts.push(MirInst::HomeStore { dest: fresh(MirTy::Bool, &mut new_types), home: h, value: zero });
                added += 2;
            }
            block.insts.push(inst);
        }
    }
    func.types.extend(new_types);
    let mut order: Vec<(u16, ValueId)> = homes.iter().map(|(&v, &h)| (h, v)).collect();
    order.sort_unstable();
    for &(h, _) in &order {
        while func.homes.len() <= usize::from(h) {
            func.homes.push(MirTy::I64);
        }
    }
    // The pad: each home's word loses this frame's owner, then the unwind
    // goes on (a body's own pad does that after its `defer`s).
    let decs = |pad: &mut MirFunc| {
        let mut ops = Vec::new();
        for &(h, _) in &order {
            let v = ValueId(pad.types.len() as u32);
            pad.types.push(MirTy::I64);
            let t = ValueId(pad.types.len() as u32);
            pad.types.push(MirTy::Bool);
            ops.push(MirInst::HomeLoad { dest: v, home: h });
            ops.push(MirInst::RcDec { dest: t, src: v, guard: None });
        }
        ops
    };
    match func.cleanup.as_deref_mut() {
        Some(pad) => {
            pad.homes.clone_from(&func.homes);
            let ops = decs(pad);
            added += ops.len();
            let entry = pad.entry.index();
            let tail = std::mem::take(&mut pad.blocks[entry].insts);
            pad.blocks[entry].insts = ops;
            pad.blocks[entry].insts.extend(tail);
        }
        None => {
            let Some(mut pad) = new_pad(func) else { return added };
            let ops = decs(&mut pad);
            added += ops.len();
            let entry = pad.entry.index();
            let tail = std::mem::take(&mut pad.blocks[entry].insts);
            pad.blocks[entry].insts = ops;
            pad.blocks[entry].insts.extend(tail);
            func.cleanup = Some(Box::new(pad));
        }
    }
    added
}

/// A cleanup pad that only resumes the unwind, over `func`'s homes.
fn new_pad(func: &MirFunc) -> Option<MirFunc> {
    let mut b = super::MirBuilder::new(format!("{}$pad", func.name));
    b.allow_host_edges = true;
    b.set_ret_ty(MirTy::I64);
    for &ty in &func.homes {
        b.add_home(ty).ok()?;
    }
    b.ins_host_invoke(common::UNWIND_RESUME_ID, Vec::new()).ok()?;
    let zero = b.ins_const(super::inst::MirConst::I64(0)).ok()?;
    b.ret(Some(zero)).ok()?;
    b.finish().ok()
}

/// Cancel an `RcInc` of a word with a later `RcDec` of the same word in its
/// block when nothing between them can decrement a count: the frame's own
/// owner is not needed while no object can be released. Returns how many
/// ops it removed.
fn cancel_pairs(func: &mut MirFunc, homes: &HashMap<ValueId, u16>) -> usize {
    // Only a word read out of a place that keeps owning it: the place's
    // owner holds the count above zero across the pair. A new object's
    // only owner is the frame, whose decrement is its release.
    let loaded: HashSet<ValueId> = func
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter_map(|inst| match inst {
            MirInst::ProductLoad { dest, .. }
            | MirInst::Index { dest, .. }
            | MirInst::LaneIndex { dest, .. }
            | MirInst::MatchPayload { dest, .. }
            | MirInst::LoadStatic { dest, .. }
            | MirInst::Cow { dest, unique: false, field: None, .. } => Some(*dest),
            _ => None,
        })
        .collect();
    let mut removed = 0;
    for block in &mut func.blocks {
        // Field stores into an object allocated earlier in the block, each
        // slot's first: the old word is no word, so nothing is released.
        let mut fresh: HashSet<ValueId> = HashSet::new();
        let mut stored: HashSet<(ValueId, u32)> = HashSet::new();
        let mut first_store = vec![false; block.insts.len()];
        for (i, inst) in block.insts.iter().enumerate() {
            match inst {
                MirInst::Alloc { dest, .. } => {
                    fresh.insert(*dest);
                }
                MirInst::HeapFieldStore { object, index, .. } => {
                    first_store[i] = fresh.contains(object) && stored.insert((*object, *index));
                }
                _ => {}
            }
        }
        let quiet = |i: usize, inst: &MirInst| match inst {
            MirInst::Const { .. }
            | MirInst::Bin { .. }
            | MirInst::Unary { .. }
            | MirInst::Cast { .. }
            | MirInst::Cmp { .. }
            | MirInst::ProductLoad { .. }
            | MirInst::Index { .. }
            | MirInst::LaneIndex { .. }
            | MirInst::MatchPayload { .. }
            | MirInst::LoadStatic { .. }
            | MirInst::HomeLoad { .. }
            | MirInst::GcBarrier { .. }
            | MirInst::Alloc { .. }
            | MirInst::String { .. }
            | MirInst::ArrayPush { .. }
            | MirInst::LanePush { .. }
            | MirInst::RcInc { .. } => true,
            MirInst::Cow { unique: false, field: None, .. } => true,
            MirInst::HeapFieldStore { .. } => first_store[i],
            _ => false,
        };
        // Another word's decrement in between moves after the pair's: an
        // owner kept longer frees nothing early.
        let mut gone = vec![false; block.insts.len()];
        let mut relocated: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut any = false;
        for i in 0..block.insts.len() {
            let MirInst::RcInc { src, guard: None, .. } = block.insts[i] else { continue };
            // A word a home keeps is stored there between the two.
            if gone[i] || homes.contains_key(&src) || !loaded.contains(&src) {
                continue;
            }
            let mut later = Vec::new();
            for j in i + 1..block.insts.len() {
                // Decrements an earlier pair moved run here.
                if relocated.contains_key(&j) {
                    break;
                }
                if gone[j] {
                    continue;
                }
                match &block.insts[j] {
                    MirInst::RcDec { src: s, guard: None, .. } if *s == src => {
                        gone[i] = true;
                        gone[j] = true;
                        removed += 2;
                        any = true;
                        for &k in &later {
                            gone[k] = true;
                        }
                        relocated.entry(j).or_default().extend(later);
                        break;
                    }
                    MirInst::RcDec { guard: None, .. } => later.push(j),
                    inst if quiet(j, inst) => {}
                    _ => break,
                }
            }
        }
        if any {
            let old = std::mem::take(&mut block.insts);
            let mut keep: Vec<Option<MirInst>> = old.into_iter().map(Some).collect();
            let mut out = Vec::with_capacity(keep.len());
            for k in 0..keep.len() {
                if !gone[k] {
                    out.push(keep[k].take().expect("each op once"));
                }
                if let Some(moved) = relocated.remove(&k) {
                    out.extend(moved.into_iter().map(|m| keep[m].take().expect("each moved op once")));
                }
            }
            block.insts = out;
        }
    }
    removed
}

struct Pass {
    /// Each counted value's root (itself for a root).
    base: HashMap<ValueId, ValueId>,
    /// Roots that are borrowed (parameters).
    borrowed: HashSet<ValueId>,
    /// Owned roots whose definition already owns its word.
    owned_at_def: HashSet<ValueId>,
    /// Words that are numbers by their definition (a constant, arithmetic,
    /// a comparison): never an object, whatever their type.
    scalar: HashSet<ValueId>,
}

/// The value an alias-defining instruction names the same object as.
fn alias_source(inst: &MirInst, counted: &dyn Fn(ValueId) -> bool) -> Option<ValueId> {
    match inst {
        MirInst::Cast { kind: MirCastKind::Word, src, .. } | MirInst::Unary { src, .. } => counted(*src).then_some(*src),
        MirInst::Cast { src, .. } => counted(*src).then_some(*src),
        // A tag bit set or cleared keeps the object; arithmetic makes a number.
        MirInst::Bin { op: MirBinOp::BitAnd | MirBinOp::BitOr | MirBinOp::Xor, lhs, rhs, .. } => [*lhs, *rhs].into_iter().find(|v| counted(*v)),
        MirInst::HeapFieldStore { value, .. } | MirInst::StoreIndex { value, .. } => counted(*value).then_some(*value),
        MirInst::ArrayPush { array, .. } | MirInst::LanePush { array, .. } | MirInst::LanesToWords { array, .. } => counted(*array).then_some(*array),
        _ => None,
    }
}

impl Pass {
    fn new(func: &MirFunc, numbers: &HashSet<Label>, returns_number: bool) -> Self {
        let pointers = pointer_words(func);
        let mut counted: HashSet<ValueId> = (0..func.types.len() as u32).map(ValueId).filter(|v| matches!(ty_of(func, *v), t if heap_word(t) || t == MirTy::Value)).collect();
        counted.extend(opaque_words(func));
        counted.extend(pair_words(func, &pointers));
        counted.extend(pointers);
        // A call of a body that only returns numbers hands back no object.
        let number_calls: HashSet<ValueId> = if numbers.is_empty() {
            HashSet::new()
        } else {
            number_words(func, numbers)
                .into_iter()
                .filter(|v| ty_of(func, *v) == MirTy::I64)
                .collect()
        };
        // An `int` word read from an object or handed back by a call may
        // be a pointer the types do not say (a pair's low word, a payload
        // of a type variable): counted, the runtime tests the address.
        let maybe = |v: ValueId| matches!(ty_of(func, v), MirTy::I64 | MirTy::Value);
        for inst in func.blocks.iter().flat_map(|b| &b.insts) {
            match inst {
                MirInst::MatchPayload { .. }
                | MirInst::ProductLoad { .. }
                | MirInst::Index { .. }
                | MirInst::LaneIndex { .. }
                | MirInst::LoadStatic { .. }
                | MirInst::HomeLoad { .. }
                | MirInst::HostInvoke { .. }
                | MirInst::Call { .. }
                | MirInst::CallIndirect { .. }
                | MirInst::ResumeCoro { .. }
                | MirInst::Yield { .. } => counted.extend(inst.dests().filter(|d| maybe(*d) && !number_calls.contains(d))),
                _ => {}
            }
        }
        // A φ that merges a counted word may hold its object.
        let mut changed = true;
        while changed {
            changed = false;
            for inst in func.blocks.iter().flat_map(|b| &b.insts) {
                if let MirInst::Phi { dest, args, .. } = inst
                    && maybe(*dest)
                    && !counted.contains(dest)
                    && args.iter().any(|(_, a)| counted.contains(a))
                {
                    counted.insert(*dest);
                    changed = true;
                }
            }
        }
        // An `int` word the frame only computes with is a number.
        let nums = used_as_numbers(func, returns_number);
        counted.retain(|v| !nums.contains(v));
        // A barrier's dest is the object the op before it made, from the
        // safepoint on; a barrier with none names no object.
        let barriers = barrier_pairs(func);
        for inst in func.blocks.iter().flat_map(|b| &b.insts) {
            if let MirInst::GcBarrier { dest, .. } = inst
                && !barriers.contains_key(dest)
            {
                counted.remove(dest);
            }
        }
        // Aliases, in definition order: a block's defs come after what
        // dominates them, and a φ is a root. Parameters are roots.
        let mut base: HashMap<ValueId, ValueId> = func.params.iter().filter(|p| counted.contains(p)).map(|&p| (p, p)).collect();
        let order = reverse_postorder(func);
        let mut owned_at_def = HashSet::new();
        for &b in &order {
            for inst in &func.block(b).insts {
                let dest = inst.dest();
                let src = match inst {
                    MirInst::GcBarrier { dest, .. } => barriers.get(dest).copied(),
                    _ => {
                        let base = &base;
                        let counted = &counted;
                        alias_source(inst, &|v| base.contains_key(&v) || counted.contains(&v))
                    }
                };
                if let Some(src) = src
                    && let Some(&root) = base.get(&src)
                {
                    base.insert(dest, root);
                    continue;
                }
                if counted.contains(&dest) {
                    base.insert(dest, dest);
                    match inst {
                        MirInst::Call { dest_hi, .. } => {
                            owned_at_def.insert(dest);
                            if let Some(h) = dest_hi
                                && counted.contains(h)
                            {
                                base.insert(*h, *h);
                                owned_at_def.insert(*h);
                            }
                        }
                        MirInst::CallIndirect { pair, .. } => {
                            owned_at_def.insert(dest);
                            if let Some((h, _)) = pair
                                && counted.contains(h)
                            {
                                base.insert(*h, *h);
                                owned_at_def.insert(*h);
                            }
                        }
                        MirInst::Phi { .. } => {
                            owned_at_def.insert(dest);
                        }
                        _ => {}
                    }
                }
                // A second word some ops define (a host call's pair tag, a
                // lane element's second word) is a root of its own.
                for d in inst.dests().skip(1) {
                    if counted.contains(&d) && !base.contains_key(&d) {
                        base.insert(d, d);
                    }
                }
            }
        }
        // A generator's parameters are its coroutine's words, which the
        // coroutine took an owner of when it was made: its frame owns them.
        let generator = super::frame_words::is_generator(func);
        let mut borrowed: HashSet<ValueId> = if generator { HashSet::new() } else { func.params.iter().copied().filter(|p| counted.contains(p)).collect() };
        for &p in func.params.iter().filter(|p| counted.contains(p)) {
            base.insert(p, p);
            if generator {
                owned_at_def.insert(p);
            }
        }
        // An enum's payload is never written after the enum is made, so a
        // payload of a borrowed enum stays owned by it for the whole call:
        // borrowed too (no count of its own).
        if !borrowed.is_empty() {
            let mut changed = true;
            while changed {
                changed = false;
                let mut payload_of = |dest: ValueId, scrutinee: ValueId, borrowed: &mut HashSet<ValueId>| {
                    if base.get(&dest) == Some(&dest)
                        && base.get(&scrutinee).is_some_and(|r| borrowed.contains(r))
                        && borrowed.insert(dest)
                    {
                        changed = true;
                    }
                };
                for block in &func.blocks {
                    for inst in &block.insts {
                        if let MirInst::MatchPayload { dest, scrutinee, .. } = inst {
                            payload_of(*dest, *scrutinee, &mut borrowed);
                        }
                    }
                    if let Some(Terminator::JumpIfMatch { scrutinee, payloads, .. }) = &block.term {
                        for &p in payloads {
                            payload_of(p, *scrutinee, &mut borrowed);
                        }
                    }
                }
            }
        }
        // A load from a borrowed holder that the frame only reads through,
        // within its block and before anything may write a word into an
        // object, is borrowed too: the holder keeps it that long.
        let mut changed = !borrowed.is_empty();
        while changed {
            changed = false;
            for (bi, block) in func.blocks.iter().enumerate() {
                for (ii, inst) in block.insts.iter().enumerate() {
                    let (dest, holder) = match inst {
                        MirInst::ProductLoad { dest, object, .. } => (*dest, *object),
                        MirInst::Index { dest, array, .. } => (*dest, *array),
                        _ => continue,
                    };
                    if base.get(&dest) != Some(&dest) || borrowed.contains(&dest) || !base.get(&holder).is_some_and(|r| borrowed.contains(r)) {
                        continue;
                    }
                    if base.iter().any(|(k, r)| *r == dest && *k != dest) {
                        continue;
                    }
                    let elsewhere = func.blocks.iter().enumerate().any(|(bj, b)| {
                        b.term.as_ref().is_some_and(|t| t.uses().any(|u| u == dest)) || (bj != bi && b.insts.iter().any(|i| i.operands().contains(&dest)))
                    });
                    if elsewhere {
                        continue;
                    }
                    let rest = &block.insts[ii + 1..];
                    let last = rest.iter().rposition(|i| i.operands().contains(&dest)).map_or(0, |j| j + 1);
                    let ok = rest[..last].iter().all(|i| if i.operands().contains(&dest) { reads_only(i, dest) } else { !may_write_words(i) });
                    if ok {
                        borrowed.insert(dest);
                        changed = true;
                    }
                }
            }
        }
        let scalar = func
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .filter(|i| match i {
                MirInst::Const { .. } => true,
                MirInst::Bin { op, .. } => !matches!(op, MirBinOp::BitAnd | MirBinOp::BitOr | MirBinOp::Xor),
                _ => false,
            })
            .map(MirInst::dest)
            .filter(|d| !base.contains_key(d))
            .collect();
        Self { base, borrowed, owned_at_def, scalar }
    }

    /// Whether `v`, which is no counted word, may still name an object the
    /// types do not say (an `int` or untyped word that is no number).
    fn maybe_object(&self, func: &MirFunc, v: ValueId) -> bool {
        matches!(ty_of(func, v), MirTy::I64 | MirTy::Value) && !self.scalar.contains(&v)
    }

    fn root(&self, v: ValueId) -> Option<ValueId> {
        self.base.get(&v).copied()
    }

    /// An owned root `v` names, if any.
    fn owned(&self, v: ValueId) -> Option<ValueId> {
        self.root(v).filter(|r| !self.borrowed.contains(r))
    }

    /// The owned roots an instruction reads.
    fn used_roots(&self, inst: &MirInst) -> Vec<ValueId> {
        if matches!(inst, MirInst::GcBarrier { .. }) {
            return Vec::new();
        }
        let mut out: Vec<ValueId> = inst.operands().into_iter().filter_map(|v| self.owned(v)).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Whether the frame reads root `r` after instruction `ii` of block `b`:
    /// a later use there or past the block. A slot clear (`ClearWord`)
    /// reads nothing, and a barrier only names live words.
    fn read_after(&self, func: &MirFunc, b: BlockId, ii: usize, r: ValueId, live_in: &[HashSet<ValueId>]) -> bool {
        let block = func.block(b);
        let names = |v: &ValueId| self.root(*v) == Some(r);
        let later = block.insts[ii + 1..].iter().any(|i| !matches!(i, MirInst::ClearWord { .. } | MirInst::GcBarrier { .. }) && i.operands().iter().any(names));
        let term = block.term.as_ref();
        later
            || term.is_some_and(|t| t.uses().any(|v| names(&v)))
            || term.is_some_and(|t| t.succs().iter().any(|s| live_in[s.index()].contains(&r) || phi_args_from(func.block(*s), b).iter().any(|(v, _)| names(v))))
    }

    fn term_roots(&self, term: &Terminator) -> Vec<ValueId> {
        let mut out: Vec<ValueId> = term.uses().filter_map(|v| self.owned(v)).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Owned roots live into each block, above its φs (a φ's args count as
    /// live at the end of their predecessor).
    fn live_in(&self, func: &MirFunc) -> Vec<HashSet<ValueId>> {
        let n = func.blocks.len();
        let mut live_in: Vec<HashSet<ValueId>> = vec![HashSet::new(); n];
        let mut changed = true;
        while changed {
            changed = false;
            for bi in (0..n).rev() {
                let mut live = self.live_end(func, BlockId(bi as u32), &live_in);
                for inst in func.blocks[bi].insts.iter().rev() {
                    for d in inst.dests() {
                        if self.root(d) == Some(d) {
                            live.remove(&d);
                        }
                    }
                    if !inst.is_phi() {
                        live.extend(self.used_roots(inst));
                    }
                }
                if live_in[bi] != live {
                    live_in[bi] = live;
                    changed = true;
                }
            }
        }
        live_in
    }

    /// Owned roots live at the end of `b`, its terminator's uses included.
    fn live_end(&self, func: &MirFunc, b: BlockId, live_in: &[HashSet<ValueId>]) -> HashSet<ValueId> {
        let mut live = HashSet::new();
        let Some(term) = &func.block(b).term else {
            return live;
        };
        live.extend(self.term_roots(term));
        for s in term.succs() {
            let sb = func.block(s);
            let phis: HashSet<ValueId> = sb.insts.iter().take_while(|i| i.is_phi()).map(MirInst::dest).collect();
            live.extend(live_in[s.index()].iter().filter(|v| !phis.contains(v)));
            for (v, _) in phi_args_from(sb, b) {
                if let Some(r) = self.owned(v) {
                    live.insert(r);
                }
            }
        }
        // The payloads a `JumpIfMatch` binds are defined on its edge.
        for v in term.binds() {
            if let Some(r) = self.root(*v) {
                live.remove(&r);
            }
        }
        live
    }

    /// Owned roots live where a panic may unwind the frame (an
    /// instruction that may trap or call), each with the home
    /// [`unwind_homes`] keeps it in.
    fn unwind_roots(&self, func: &MirFunc, live_in: &[HashSet<ValueId>]) -> HashMap<ValueId, u16> {
        let mut out: HashMap<ValueId, u16> = HashMap::new();
        if super::frame_words::is_generator(func) {
            return out;
        }
        let storable = |r: ValueId| matches!(ty_of(func, r), MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes | MirTy::I64);
        let mut found: Vec<ValueId> = Vec::new();
        for (bi, block) in func.blocks.iter().enumerate() {
            let mut live = self.live_end(func, BlockId(bi as u32), live_in);
            for inst in block.insts.iter().rev() {
                for d in inst.dests() {
                    if self.root(d) == Some(d) {
                        live.remove(&d);
                    }
                }
                if !inst.is_phi() {
                    let used = self.used_roots(inst);
                    if may_unwind(inst) {
                        found.extend(live.iter().copied().chain(used.iter().copied()).filter(|&r| storable(r)));
                    }
                    live.extend(used);
                }
            }
        }
        found.sort_unstable();
        found.dedup();
        let base = func.homes.len();
        for (k, r) in found.into_iter().enumerate() {
            let Ok(h) = u16::try_from(base + k) else { break };
            out.insert(r, h);
        }
        out
    }

    fn run(&self, func: &mut MirFunc, unwinds: bool) -> (usize, HashMap<ValueId, u16>) {
        if self.base.is_empty() {
            return (0, HashMap::new());
        }
        let live_in = self.live_in(func);
        // `COIL_RC_UNWIND=0`: no homes (an unwound frame leaks what it owned).
    let homes = if unwinds && unwind_homes_on() { self.unwind_roots(func, &live_in) } else { HashMap::new() };
        let n = func.blocks.len();
        let mut new_types: Vec<MirTy> = Vec::new();
        let mut next = func.types.len() as u32;
        let mut tok = |new_types: &mut Vec<MirTy>| {
            let v = ValueId(next);
            next += 1;
            new_types.push(MirTy::Bool);
            v
        };
        let mut inserted = 0usize;
        // In-block code: after each instruction, and before the terminator.
        let mut after: Vec<HashMap<usize, Vec<MirInst>>> = vec![HashMap::new(); n];
        let mut before: Vec<HashMap<usize, Vec<MirInst>>> = vec![HashMap::new(); n];
        let mut before_term: Vec<Vec<MirInst>> = vec![Vec::new(); n];
        // Edge code: (pred, succ) -> ops.
        let mut edges: Vec<((BlockId, BlockId), Vec<MirInst>)> = Vec::new();
        for bi in 0..n {
            let block = &func.blocks[bi];
            let b = block.id;
            let Some(term) = block.term.clone() else { continue };
            let binds: HashSet<ValueId> = term.binds().iter().copied().collect();
            // Backward: owned roots live after each instruction.
            let end = self.live_end(func, b, &live_in);
            let mut live_after: Vec<HashSet<ValueId>> = vec![HashSet::new(); block.insts.len()];
            let mut live = end.clone();
            for (ii, inst) in block.insts.iter().enumerate().rev() {
                live_after[ii] = live.clone();
                for d in inst.dests() {
                    if self.root(d) == Some(d) {
                        live.remove(&d);
                    }
                }
                if !inst.is_phi() {
                    live.extend(self.used_roots(inst));
                }
            }
            // Forward: increments of new owned roots, decrements at last uses.
            for (ii, inst) in block.insts.iter().enumerate() {
                let mut ops = Vec::new();
                for d in inst.dests() {
                    if self.root(d) != Some(d) || self.borrowed.contains(&d) || binds.contains(&d) {
                        continue;
                    }
                    if !self.owned_at_def.contains(&d) && !inst.is_phi() {
                        ops.push(MirInst::RcInc { dest: tok(&mut new_types), src: d, guard: None });
                    }
                    if let Some(&h) = homes.get(&d) {
                        let w = if ty_of(func, d) == MirTy::I64 {
                            d
                        } else {
                            let w = tok(&mut new_types);
                            *new_types.last_mut().expect("the word's type") = MirTy::I64;
                            ops.push(MirInst::Cast { dest: w, kind: MirCastKind::Word, to: MirTy::I64, src: d });
                            w
                        };
                        ops.push(MirInst::HomeStore { dest: tok(&mut new_types), home: h, value: w });
                    }
                    if !live_after[ii].contains(&d) {
                        ops.push(MirInst::RcDec { dest: tok(&mut new_types), src: d, guard: None });
                    }
                }
                if !inst.is_phi() && !is_tail_call_inst(func, block, inst) {
                    for r in self.used_roots(inst) {
                        if !live_after[ii].contains(&r) {
                            ops.push(MirInst::RcDec { dest: tok(&mut new_types), src: r, guard: None });
                        }
                    }
                }
                // A write check of a word the frame reads again after it
                // (another name of the same word: a share's copy folded
                // into its source): the frame holds a second owner across
                // the check, so the object is copied, not written in place
                // under that name (reference-count plan step 5).
                if let MirInst::Cow { src, unique: true, field: None, .. } = inst
                    && let Some(r) = self.root(*src)
                    && (self.borrowed.contains(&r) || (live_after[ii].contains(&r) && self.read_after(func, b, ii, r, &live_in)))
                {
                    before[bi].entry(ii).or_default().push(MirInst::RcInc { dest: tok(&mut new_types), src: *src, guard: None });
                    ops.insert(0, MirInst::RcDec { dest: tok(&mut new_types), src: *src, guard: None });
                }
                // A static owns its word: a store reads the old one first
                // and releases it after. A home keeps its word (a `defer`
                // flag's slot may hold no word yet: its old one is not
                // released, a leak).
                if let MirInst::StoreStatic { value, .. } | MirInst::HomeStore { value, .. } = inst
                    && (self.root(*value).is_some() || self.maybe_object(func, *value))
                {
                    ops.insert(0, MirInst::RcInc { dest: tok(&mut new_types), src: *value, guard: None });
                    if let MirInst::StoreStatic { slot, value, .. } = inst {
                        let old = tok(&mut new_types);
                        *new_types.last_mut().expect("the token's type") = ty_of(func, *value);
                        before[bi].insert(ii, vec![MirInst::LoadStatic { dest: old, slot: *slot }]);
                        ops.insert(1, MirInst::RcDec { dest: tok(&mut new_types), src: old, guard: None });
                    }
                }
                if !ops.is_empty() {
                    inserted += ops.len();
                    after[bi].insert(ii, ops);
                }
            }
            // The terminator.
            match &term {
                Terminator::Return { lo, hi } => {
                    let mut moved: HashSet<ValueId> = HashSet::new();
                    let tail = block.insts.last().is_some_and(|i| is_tail_call_inst(func, block, i));
                    for v in [*lo, *hi].into_iter().flatten() {
                        match self.owned(v) {
                            Some(r) if end.contains(&r) && moved.insert(r) => {}
                            _ if tail => {}
                            Some(_) => before_term[bi].push(MirInst::RcInc { dest: tok(&mut new_types), src: v, guard: None }),
                            None => {
                                let counted = self.root(v).is_some();
                                if counted || self.maybe_object(func, v) {
                                    before_term[bi].push(MirInst::RcInc { dest: tok(&mut new_types), src: v, guard: None });
                                }
                            }
                        }
                    }
                    // Anything else live at the end dies here (a root the
                    // return reads twice is moved once).
                    for r in &end {
                        if !moved.contains(r) && !tail {
                            before_term[bi].push(MirInst::RcDec { dest: tok(&mut new_types), src: *r, guard: None });
                        }
                    }
                    inserted += before_term[bi].len();
                }
                Terminator::Unreachable => {}
                _ => {
                    let mut succs = term.succs();
                    succs.dedup();
                    for s in succs {
                        let sb = func.block(s);
                        let phis: HashSet<ValueId> = sb.insts.iter().take_while(|i| i.is_phi()).map(MirInst::dest).collect();
                        let into: HashSet<ValueId> = live_in[s.index()].iter().filter(|v| !phis.contains(v)).copied().collect();
                        let mut ops = Vec::new();
                        // The payloads bound on the taken edge are loads.
                        if let Terminator::JumpIfMatch { taken, payloads, .. } = &term
                            && *taken == s
                        {
                            let args: HashSet<ValueId> = phi_args_from(sb, b).into_iter().map(|(v, _)| v).collect();
                            for &p in payloads {
                                if self.root(p) == Some(p) && !self.borrowed.contains(&p) && (into.contains(&p) || args.contains(&p)) {
                                    ops.push(MirInst::RcInc { dest: tok(&mut new_types), src: p, guard: None });
                                    if let Some(&h) = homes.get(&p) {
                                        let w = if ty_of(func, p) == MirTy::I64 {
                                            p
                                        } else {
                                            let w = tok(&mut new_types);
                                            *new_types.last_mut().expect("the word's type") = MirTy::I64;
                                            ops.push(MirInst::Cast { dest: w, kind: MirCastKind::Word, to: MirTy::I64, src: p });
                                            w
                                        };
                                        ops.push(MirInst::HomeStore { dest: tok(&mut new_types), home: h, value: w });
                                    }
                                }
                            }
                        }
                        // Each φ arg: moved when its root dies on this edge
                        // (once), else incremented.
                        let mut moved: HashSet<ValueId> = HashSet::new();
                        for (v, phi) in phi_args_from(sb, b) {
                            // A φ that is no root (a number) owns nothing.
                            if self.root(phi) != Some(phi) {
                                continue;
                            }
                            match self.owned(v) {
                                // A bound payload was incremented above when it
                                // flows on: the φ takes that owner.
                                Some(r) if !into.contains(&r) && moved.insert(r) => {
                                    // The φ's home takes it over.
                                    if let Some(&h) = homes.get(&r) {
                                        let zero = tok(&mut new_types);
                                        *new_types.last_mut().expect("the zero's type") = MirTy::I64;
                                        ops.push(MirInst::Const { dest: zero, c: super::inst::MirConst::I64(0) });
                                        ops.push(MirInst::HomeStore { dest: tok(&mut new_types), home: h, value: zero });
                                    }
                                }
                                _ if self.root(v).is_none() && !self.maybe_object(func, v) => {}
                                _ => ops.push(MirInst::RcInc { dest: tok(&mut new_types), src: v, guard: None }),
                            }
                        }
                        // Owned roots that die on this edge.
                        let mut dying: Vec<ValueId> = end.iter().filter(|r| !into.contains(r) && !moved.contains(r) && !binds.contains(r)).copied().collect();
                        dying.sort_unstable();
                        for r in dying {
                            ops.push(MirInst::RcDec { dest: tok(&mut new_types), src: r, guard: None });
                        }
                        if !ops.is_empty() {
                            inserted += ops.len();
                            edges.push(((b, s), ops));
                        }
                    }
                }
            }
        }
        func.types.extend(new_types);
        // Materialize: in-block code first (positions are the old ones).
        for (bi, block) in func.blocks.iter_mut().enumerate() {
            if after[bi].is_empty() && before[bi].is_empty() && before_term[bi].is_empty() {
                continue;
            }
            let old = std::mem::take(&mut block.insts);
            // An allocation's barrier stays right after it: ops after the
            // allocation go past its barrier.
            // A run of field stores into one object (a flat pair field's
            // word, then its tag) counts the word once its tag says it is a
            // pointer: decrements wait for the run's end.
            let hold: Vec<bool> = old
                .iter()
                .zip(old.iter().skip(1))
                .map(|(a, b)| match (a, b) {
                    (_, MirInst::GcBarrier { .. }) => true,
                    (MirInst::HeapFieldStore { object: o1, .. }, MirInst::HeapFieldStore { object: o2, .. }) => o1 == o2,
                    _ => false,
                })
                .collect();
            let mut pending: Vec<MirInst> = Vec::new();
            for (ii, inst) in old.into_iter().enumerate() {
                if let Some(ops) = before[bi].remove(&ii) {
                    block.insts.extend(ops);
                }
                block.insts.push(inst);
                if let Some(ops) = after[bi].remove(&ii) {
                    pending.extend(ops);
                }
                if !hold.get(ii).copied().unwrap_or(false) {
                    block.insts.append(&mut pending);
                }
            }
            block.insts.append(&mut pending);
            // φs stay first: ops after a φ go past the last φ.
            hoist_phis(block);
            block.insts.append(&mut before_term[bi]);
        }
        for ((p, s), ops) in edges {
            place_edge(func, p, s, ops);
        }
        (inserted, homes)
    }
}

/// Whether a panic may unwind the frame at `inst`: it calls, traps or may
/// run code that does.
fn may_unwind(inst: &MirInst) -> bool {
    !matches!(
        inst,
        MirInst::Const { .. }
            | MirInst::Unary { .. }
            | MirInst::Cast { .. }
            | MirInst::Cmp { .. }
            | MirInst::Phi { .. }
            | MirInst::ProductLoad { .. }
            | MirInst::MatchPayload { .. }
            | MirInst::LoadStatic { .. }
            | MirInst::StoreStatic { .. }
            | MirInst::HomeLoad { .. }
            | MirInst::HomeStore { .. }
            | MirInst::GcBarrier { .. }
            | MirInst::Alloc { .. }
            | MirInst::String { .. }
            | MirInst::RcInc { .. }
            | MirInst::RcDec { .. }
            | MirInst::HeapFieldStore { .. }
            | MirInst::ArrayPush { .. }
            | MirInst::LanePush { .. }
            | MirInst::Cow { unique: false, field: None, .. }
    ) && !matches!(inst, MirInst::Bin { op, .. } if !matches!(op, MirBinOp::Div | MirBinOp::Rem | MirBinOp::Pow))
}

/// Move a block's φs back in front of anything inserted between them.
fn hoist_phis(block: &mut MirBlock) {
    if block.insts.iter().skip_while(|i| i.is_phi()).any(MirInst::is_phi) {
        let (phis, rest): (Vec<MirInst>, Vec<MirInst>) = std::mem::take(&mut block.insts).into_iter().partition(MirInst::is_phi);
        block.insts = phis;
        block.insts.extend(rest);
    }
}

/// The args `succ`'s φs take from `pred`, with each φ's dest.
fn phi_args_from(succ: &MirBlock, pred: BlockId) -> Vec<(ValueId, ValueId)> {
    succ.insts
        .iter()
        .take_while(|i| i.is_phi())
        .filter_map(|i| match i {
            MirInst::Phi { dest, args, .. } => args.iter().find(|(p, _)| *p == pred).map(|(_, v)| (*v, *dest)),
            _ => None,
        })
        .collect()
}

/// Put `ops` on the edge `pred -> succ`: at the end of `pred` when it has
/// no other way out, at the start of `succ` (past its φs) when it has no
/// other way in, else in a block of their own on the edge.
fn place_edge(func: &mut MirFunc, pred: BlockId, succ: BlockId, ops: Vec<MirInst>) {
    let single_succ = matches!(func.block(pred).term, Some(Terminator::Jump { .. }));
    if single_succ {
        let b = func.block_mut(pred);
        b.insts.extend(ops);
        return;
    }
    let edges_in: usize = func
        .blocks
        .iter()
        .filter_map(|b| b.term.as_ref())
        .map(|t| t.succs().into_iter().filter(|s| *s == succ).count())
        .sum();
    if edges_in == 1 && succ != func.entry {
        let b = func.block_mut(succ);
        let at = b.insts.iter().take_while(|i| i.is_phi()).count();
        b.insts.splice(at..at, ops);
        return;
    }
    let id = BlockId(func.blocks.len() as u32);
    let mut block = MirBlock::new(id);
    block.insts = ops;
    block.term = Some(Terminator::Jump { dest: succ });
    func.blocks.push(block);
    if let Some(term) = func.block_mut(pred).term.as_mut() {
        match term {
            Terminator::Br { taken, not_taken, .. } | Terminator::JumpIfMatch { taken, not_taken, .. } => {
                for t in [taken, not_taken] {
                    if *t == succ {
                        *t = id;
                    }
                }
            }
            Terminator::Jump { dest } if *dest == succ => *dest = id,
            _ => {}
        }
    }
    for inst in func.block_mut(succ).insts.iter_mut() {
        let MirInst::Phi { args, .. } = inst else { break };
        for (p, _) in args.iter_mut() {
            if *p == pred {
                *p = id;
            }
        }
    }
}

/// The blocks reachable from the entry, each after its dominators.
fn reverse_postorder(func: &MirFunc) -> Vec<BlockId> {
    let n = func.blocks.len();
    let mut seen = vec![false; n];
    let mut post = Vec::with_capacity(n);
    let mut stack: Vec<(BlockId, usize)> = vec![(func.entry, 0)];
    if n == 0 {
        return post;
    }
    seen[func.entry.index()] = true;
    while let Some((b, i)) = stack.pop() {
        let succs = func.block(b).term.as_ref().map(Terminator::succs).unwrap_or_default();
        if i < succs.len() {
            stack.push((b, i + 1));
            let s = succs[i];
            if s.index() < n && !seen[s.index()] {
                seen[s.index()] = true;
                stack.push((s, 0));
            }
        } else {
            post.push(b);
        }
    }
    post.reverse();
    post
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rc_ops(f: &MirFunc) -> Vec<&MirInst> {
        f.blocks[0].insts.iter().filter(|i| matches!(i, MirInst::RcInc { .. } | MirInst::RcDec { .. })).collect()
    }

    /// An inc and a dec of one word with nothing between them that can
    /// release an object cancel; another word's dec between them moves
    /// after the pair's place; a call between them keeps both.
    #[test]
    fn a_pair_with_no_release_between_cancels() {
        let quiet = "func @f(v0: heapref, v2: heapref) -> heapref {\nbb0:\n    v1 = productload v2, 0\n    v3 = rc.inc v1\n    v4 = arraypush v0, v1\n    v5 = rc.dec v2\n    v6 = productload v1, 0\n    v7 = rc.dec v1\n    return v0\n}\n";
        let mut f = crate::mir::parse_func(quiet).expect(quiet);
        assert_eq!(cancel_pairs(&mut f, &HashMap::new()), 2);
        assert_eq!(rc_ops(&f).len(), 1, "{f}");
        let pos = |f: &MirFunc, pred: &dyn Fn(&MirInst) -> bool| f.blocks[0].insts.iter().position(pred).expect("op");
        assert!(pos(&f, &|i| matches!(i, MirInst::RcDec { .. })) > pos(&f, &|i| matches!(i, MirInst::ProductLoad { .. })), "{f}");
        let call = quiet.replace("v6 = productload v1, 0", "v6 = call.heapref 9 v1");
        let mut g = crate::mir::parse_func(&call).expect(&call);
        assert_eq!(cancel_pairs(&mut g, &HashMap::new()), 0);
        assert_eq!(rc_ops(&g).len(), 3, "{g}");
    }

    fn all_rc_ops(f: &MirFunc) -> usize {
        f.blocks.iter().flat_map(|b| &b.insts).filter(|i| matches!(i, MirInst::RcInc { .. } | MirInst::RcDec { .. })).count()
    }

    /// A tree walk: the payloads of the borrowed enum are borrowed too, and
    /// the recursive call returns only numbers, so nothing is counted.
    #[test]
    fn a_walk_of_a_borrowed_enum_counts_nothing() {
        let walk = "func @w(v0: heapref) -> i64 {\nbb0:\n    jumpifmatch v0, 0, bb2, bb1\nbb1:\n    v1 = matchpayload v0, 0\n    v2 = call.i64 7 v1\n    v3 = iconst.i64 1\n    v4 = iadd v2, v3\n    return v4\nbb2:\n    v5 = iconst.i64 1\n    return v5\n}\n";
        let mut f = crate::mir::parse_func(walk).expect(walk);
        insert_rc_all([(Some(Label(7)), &mut f)]);
        assert_eq!(all_rc_ops(&f), 0, "{f}");
        // Called under another label, its result is a word to count.
        let mut g = crate::mir::parse_func(walk).expect(walk);
        insert_rc_all([(Some(Label(8)), &mut g)]);
        assert_eq!(all_rc_ops(&g), 1, "{g}");
    }

    /// An `int` field the frame only adds up is no word to count; one it
    /// hands on (here returned) is.
    #[test]
    fn an_int_field_summed_counts_nothing() {
        let sum = "func @s(v0: heapref, v1: i64) -> i64 {\nbb0:\n    v2 = productload v0, 0\n    v3 = iadd v1, v2\n    return v3\n}\n";
        let mut f = crate::mir::parse_func(sum).expect(sum);
        insert_rc_all([(None, &mut f)]);
        assert_eq!(all_rc_ops(&f), 0, "{f}");
        let kept = "func @k(v0: heapref) -> i64 {\nbb0:\n    v2 = productload v0, 0\n    return v2\n}\n";
        let mut g = crate::mir::parse_func(kept).expect(kept);
        insert_rc_all([(None, &mut g)]);
        assert!(all_rc_ops(&g) > 0, "{g}");
    }

    /// A field of a borrowed object handed to a host native that only
    /// reads it borrows the object's count; handed to a call, which may
    /// overwrite the field, it takes one of its own.
    #[test]
    fn a_borrowed_field_read_by_a_host_counts_nothing() {
        let read = "func @r(v0: heapref, v1: i64) -> i64 {\nbb0:\n    v2 = productload v0, 0\n    v3 = host.key_index_probe v2, v2, v1, v1\n    v4 = iadd v3, v1\n    return v4\n}\n";
        let mut f = crate::mir::parse_func(read).expect(read);
        insert_rc_all([(None, &mut f)]);
        assert_eq!(all_rc_ops(&f), 0, "{f}");
        let call = "func @c(v0: heapref, v1: i64) -> i64 {\nbb0:\n    v2 = productload v0, 0\n    v3 = call.i64 7 v2\n    v4 = iadd v3, v1\n    return v4\n}\n";
        let mut g = crate::mir::parse_func(call).expect(call);
        insert_rc_all([(None, &mut g)]);
        assert!(all_rc_ops(&g) > 0, "{g}");
    }

    #[test]
    fn a_new_objects_pair_stays_its_release() {
        let src = "func @f(v0: i64) -> i64 {\nbb0:\n    v1 = alloc.enum[1] 0, v0\n    v2 = rc.inc v1\n    v3 = matchpayload v1, 0\n    v4 = rc.dec v1\n    return v3\n}\n";
        let mut f = crate::mir::parse_func(src).expect(src);
        assert_eq!(cancel_pairs(&mut f, &HashMap::new()), 0);
        assert_eq!(rc_ops(&f).len(), 2, "{f}");
    }
}
