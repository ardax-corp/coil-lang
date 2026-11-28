//! Reference count insertion (reference counts plan step 4): the
//! `RcInc` / `RcDec` ops that make a frame own the objects its words name.
//!
//! Each value that may hold an object (a heap word, or an `int` word the
//! collector roots: [`pointer_words`], [`opaque_words`], [`maybe_obj_words`]) is
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

use super::frame_words::{barrier_pairs, heap_word, maybe_obj_words, opaque_words, pointer_words, ty_of};
use super::func::{MirBlock, MirFunc, is_tail_call_inst};
use super::inst::{BlockId, MirBinOp, MirCastKind, MirInst, Terminator, ValueId};
use super::ty::MirTy;

/// Whether an unwound frame releases what it owned ([`unwind_homes`]):
/// on unless `COIL_RC_UNWIND=0`.
pub(super) fn unwind_homes_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("COIL_RC_UNWIND").map_or(true, |v| v != "0"))
}

/// Insert the count ops into a program's bodies, each `(entry, body)`, and
/// their cleanup pads. A call to a body that only ever returns a number
/// gives no word to count. The bodies in `owning` (the tail twins
/// [`owning_tail_twins`] made) own their parameters. Returns how many ops
/// it inserted.
pub fn insert_rc_all<'a>(bodies: impl IntoIterator<Item = (Option<Label>, &'a mut MirFunc)>, twins: &HashMap<Label, Label>) -> usize {
    let mut bodies: Vec<(Option<Label>, &mut MirFunc)> = bodies.into_iter().collect();
    let owning: HashSet<Label> = twins.values().copied().collect();
    let numbers = number_returns(bodies.iter().map(|(l, f)| (*l, &**f)));
    let wide_numbers = number_return_words(bodies.iter().map(|(l, f)| (*l, &**f)), &numbers);
    let mut coros = owning_bodies(bodies.iter().map(|(_, f)| &**f));
    coros.extend(owning.iter().copied());
    let quiet = quiet_bodies(bodies.iter().map(|(l, f)| (*l, &**f)), &coros);
    let mut facts = Facts { numbers, wide_numbers, quiet, owned_params: HashMap::new(), twin_of: twins.clone() };
    // Which parameters a tail twin releases: its caller hands each an owner.
    let owned_params: HashMap<Label, Vec<bool>> = bodies
        .iter()
        .filter_map(|(l, f)| {
            let l = (*l).filter(|l| owning.contains(l))?;
            let pass = Pass::new(f, &facts, false, true);
            Some((l, f.params.iter().map(|&p| pass.root(p) == Some(p)).collect()))
        })
        .collect();
    facts.owned_params = owned_params;
    bodies.iter_mut().map(|(l, f)| insert_rc_in(f, false, &facts, *l, l.is_some_and(|l| coros.contains(&l)))).sum()
}

/// A tail call hands its callee the frame, so the caller can release
/// nothing after it: what it passes would leak. Each body a tail call
/// targets that takes a parameter that may hold an object gets a twin
/// that owns its parameters (`name$own`, a fresh label), and every tail
/// call to it, in its twin too, enters the twin with its arguments'
/// owners handed over.
///
/// A body a direct call targets that takes a parameter apart and builds
/// objects ([`consumes`]) gets one too: a call whose argument dies there
/// enters the twin and moves it in ([`Pass::run`]), so the body owns what
/// it takes apart and may reuse its memory. The body itself keeps borrowed
/// parameters for every caller that cannot know (the runtime's entries,
/// call folding, function values). Returns each twin by its body.
pub fn owning_tail_twins(bodies: &mut Vec<super::native_capture::NativeBody>) -> HashMap<Label, Label> {
    let by_label: HashMap<Label, usize> = bodies.iter().enumerate().filter_map(|(i, b)| Some((b.entry?, i))).collect();
    let may_hold = |f: &MirFunc| {
        f.params.iter().any(|&p| matches!(ty_of(f, p), MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes | MirTy::MaybeObj))
            || (f.opaque_words && !f.params.is_empty())
    };
    let mut targets: Vec<Label> = Vec::new();
    for b in bodies.iter() {
        for block in &b.mir.blocks {
            if let Some(inst @ MirInst::Call { target, .. }) = block.insts.last()
                && is_tail_call_inst(&b.mir, block, inst)
                && let Some(&callee) = by_label.get(target)
                && !targets.contains(target)
            {
                let c = &bodies[callee];
                if !c.name.ends_with("::drop") && !super::frame_words::is_generator(&c.mir) && may_hold(&c.mir) {
                    targets.push(*target);
                }
            }
        }
    }
    // Bodies a direct call targets that take a parameter apart.
    let mut direct: Vec<Label> = bodies.iter().flat_map(|b| &b.mir.blocks).flat_map(|bl| &bl.insts).filter_map(|i| match i {
        MirInst::Call { target, .. } => Some(*target),
        _ => None,
    }).collect();
    direct.sort_unstable_by_key(|l| l.0);
    direct.dedup();
    for t in direct {
        let Some(&i) = by_label.get(&t) else { continue };
        let c = &bodies[i];
        if !targets.contains(&t) && !c.name.ends_with("::drop") && !super::frame_words::is_generator(&c.mir) && may_hold(&c.mir) && consumes(&c.mir) {
            targets.push(t);
        }
    }
    if targets.is_empty() {
        return HashMap::new();
    }
    let first = bodies
        .iter()
        .flat_map(|b| b.entry.into_iter().chain(b.mir.code_targets()))
        .map(|l| l.0)
        .max()
        .unwrap_or(0)
        + 1;
    let mut twin_of: HashMap<Label, Label> = HashMap::new();
    for (next, t) in (first..).zip(&targets) {
        let body = &bodies[by_label[t]];
        let twin = Label(next);
        twin_of.insert(*t, twin);
        bodies.push(super::native_capture::NativeBody { name: format!("{}$own", body.name), entry: Some(twin), mir: body.mir.clone() });
    }
    // A call may pass fewer words than its callee takes (the ones it never
    // reads): the twin, which releases its parameters, takes every one.
    let arity: HashMap<Label, usize> = twin_of.keys().map(|t| (*t, bodies[by_label[t]].mir.params.len())).collect();
    for b in bodies.iter_mut() {
        let f = &mut b.mir;
        let mut redirected = Vec::new();
        for bi in 0..f.blocks.len() {
            let block = &f.blocks[bi];
            let Some(inst @ MirInst::Call { target, args, .. }) = block.insts.last() else { continue };
            if !is_tail_call_inst(f, block, inst) || arity.get(target).is_some_and(|&n| args.len() < n) {
                continue;
            }
            let Some(&twin) = twin_of.get(target) else { continue };
            if let Some(MirInst::Call { target, .. }) = f.blocks[bi].insts.last_mut() {
                *target = twin;
            }
            redirected.push(twin);
        }
        f.tail_targets.extend(redirected);
    }
    twin_of
}

/// Whether `f` takes a parameter apart (matches on it or reads its
/// payload) and builds an enum or an instance: owning the parameter, it
/// may reuse the memory of the object it takes apart.
fn consumes(f: &MirFunc) -> bool {
    let params: HashSet<ValueId> = f.params.iter().copied().filter(|&p| matches!(ty_of(f, p), MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes | MirTy::I64 | MirTy::MaybeObj | MirTy::Value)).collect();
    if params.is_empty() {
        return false;
    }
    let builds = f.blocks.iter().flat_map(|b| &b.insts).any(|i| matches!(i, MirInst::Alloc { kind: super::MirAllocKind::Enum { .. } | super::MirAllocKind::Object { .. }, .. }));
    builds
        && f.blocks.iter().any(|b| {
            matches!(&b.term, Some(Terminator::JumpIfMatch { scrutinee, .. }) if params.contains(scrutinee))
                || b.insts.iter().any(|i| matches!(i, MirInst::MatchPayload { scrutinee, .. } | MirInst::ProductLoad { object: scrutinee, .. } if params.contains(scrutinee)))
        })
}

/// What the pass knows of the other bodies.
struct Facts {
    /// The bodies that return only numbers ([`number_returns`]).
    numbers: HashSet<Label>,
    /// Per body returning more than two words, not all numbers: which of
    /// them are ([`number_return_words`]).
    wide_numbers: HashMap<Label, Vec<bool>>,
    /// The bodies that write no word into an object ([`quiet_bodies`]).
    quiet: HashSet<Label>,
    /// The tail twins ([`owning_tail_twins`]): for each parameter, whether
    /// the twin releases it, so a tail call hands it an owner.
    owned_params: HashMap<Label, Vec<bool>>,
    /// Each body's owning twin: a direct call that moves an argument
    /// enters it.
    twin_of: HashMap<Label, Label>,
}

/// The calls of `func` that write no word into an object: of a pure
/// function, or of a body in `quiet`.
fn quiet_calls(func: &MirFunc, quiet: &HashSet<Label>) -> HashSet<ValueId> {
    let mut out = func.pure_calls.clone();
    out.extend(func.blocks.iter().flat_map(|b| &b.insts).filter_map(|i| match i {
        MirInst::Call { dest, target, .. } if quiet.contains(target) => Some(*dest),
        _ => None,
    }));
    out
}

/// The entries of the bodies that write no word into an object and call
/// only such bodies (cleanup pads included), found optimistically. A
/// body that owns its parameters is never one: it releases them.
fn quiet_bodies<'a>(bodies: impl Iterator<Item = (Option<Label>, &'a MirFunc)>, owning: &HashSet<Label>) -> HashSet<Label> {
    let cands: Vec<(Label, &MirFunc)> = bodies.filter_map(|(l, f)| Some((l?, f))).filter(|(l, f)| !owning.contains(l) && !super::frame_words::is_generator(f)).collect();
    let mut set: HashSet<Label> = cands.iter().map(|(l, _)| *l).collect();
    loop {
        let before = set.len();
        for (l, f) in &cands {
            if !set.contains(l) {
                continue;
            }
            let mut quiet = true;
            let mut body = Some(*f);
            while let Some(g) = body {
                let calls = quiet_calls(g, &set);
                quiet &= g.blocks.iter().flat_map(|b| &b.insts).all(|i| !may_write_words(i, &calls));
                body = g.cleanup.as_deref();
            }
            if !quiet {
                set.remove(l);
            }
        }
        if set.len() == before {
            return set;
        }
    }
}

/// The bodies that own their parameters: a coroutine's (a `gen fn`'s,
/// `MakeCoro` targets), whose parameters are the coroutine's words, also
/// when the body never yields; and a two-word value call's `unpair`, handed
/// the call's one-word result, which it takes over.
fn owning_bodies<'a>(bodies: impl Iterator<Item = &'a MirFunc>) -> HashSet<Label> {
    bodies
        .flat_map(|f| f.blocks.iter().flat_map(|b| &b.insts))
        .filter_map(|i| match i {
            MirInst::Alloc { kind: super::inst::MirAllocKind::Coro { target }, .. } => Some(*target),
            MirInst::CallIndirect { pair: Some((_, unpair)), .. } => Some(*unpair),
            _ => None,
        })
        .collect()
}

/// The entries of the bodies whose every return is a number (by its
/// definition: a constant, arithmetic, a comparison, a call of another
/// such body, a φ of those), found optimistically: every candidate is
/// assumed until one of its returns says otherwise.
fn number_returns<'a>(bodies: impl Iterator<Item = (Option<Label>, &'a MirFunc)>) -> HashSet<Label> {
    let cands: Vec<(Label, &MirFunc)> = bodies
        .filter_map(|(l, f)| Some((l?, f)))
        .filter(|(_, f)| (f.ret_ty == Some(MirTy::I64) || wide_scalar_ret(f)) && !super::frame_words::is_generator(f) && f.blocks.iter().any(|b| matches!(b.term, Some(Terminator::Return { .. }))))
        .collect();
    let mut set: HashSet<Label> = cands.iter().map(|(l, _)| *l).collect();
    loop {
        let before = set.len();
        for (l, f) in &cands {
            if !set.contains(l) {
                continue;
            }
            let nums = number_words(f, &set, &HashMap::new());
            let used = used_as_numbers(f, true);
            let all = f.blocks.iter().all(|b| match &b.term {
                // A wide return's words are numbers only by their definition
                // (a scalar or a number), never by their use.
                Some(Terminator::Return { lo, hi, more }) if !more.is_empty() => {
                    lo.iter().chain(hi.iter()).chain(more.iter()).all(|v| !ty_of(f, *v).is_int() || nums.contains(v))
                }
                Some(Terminator::Return { lo, hi, .. }) => hi.is_none() && lo.is_none_or(|v| nums.contains(&v) || used.contains(&v) && computed_with(f, v)),
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

/// Per body returning more than two words that is not in `numbers` (a
/// heap word among them): for each word, whether every return gives a
/// number there (by its definition, as [`number_returns`]' wide words).
fn number_return_words<'a>(bodies: impl Iterator<Item = (Option<Label>, &'a MirFunc)>, numbers: &HashSet<Label>) -> HashMap<Label, Vec<bool>> {
    let mut out = HashMap::new();
    for (l, f) in bodies {
        let Some(l) = l else { continue };
        if f.ret_more_tys.is_empty() || numbers.contains(&l) || super::frame_words::is_generator(f) {
            continue;
        }
        let nums = number_words(f, numbers, &HashMap::new());
        let tys: Vec<MirTy> = f.ret_ty.into_iter().chain(f.ret_hi_ty).chain(f.ret_more_tys.iter().copied()).collect();
        let mut mask: Vec<bool> = tys.iter().map(|t| t.is_int()).collect();
        for b in &f.blocks {
            if let Some(Terminator::Return { lo, hi, more }) = &b.term {
                for (k, v) in lo.iter().chain(hi.iter()).chain(more.iter()).enumerate() {
                    if k < mask.len() && !nums.contains(v) {
                        mask[k] = false;
                    }
                }
            }
        }
        if mask.iter().any(|&m| m) {
            out.insert(l, mask);
        }
    }
    out
}

/// `f` returns more than two words, none of them a heap word
/// ([`super::wide_ret`]'s twins).
fn wide_scalar_ret(f: &MirFunc) -> bool {
    !f.ret_more_tys.is_empty() && f.ret_ty.into_iter().chain(f.ret_hi_ty).chain(f.ret_more_tys.iter().copied()).all(|t| matches!(t, MirTy::I64 | MirTy::F64 | MirTy::Bool))
}

/// The words of `func` that are numbers by their definition, a call of a
/// body in `numbers` included, and the words of a wide result `wide` says
/// are numbers (φs and bit ops optimistically).
fn number_words(func: &MirFunc, numbers: &HashSet<Label>, wide: &HashMap<Label, Vec<bool>>) -> HashSet<ValueId> {
    let insts = || func.blocks.iter().flat_map(|b| &b.insts);
    let mut out: HashSet<ValueId> = insts()
        .filter(|i| match i {
            MirInst::Const { .. } | MirInst::Cmp { .. } => true,
            MirInst::Bin { op, .. } => !matches!(op, MirBinOp::BitAnd | MirBinOp::BitOr | MirBinOp::Xor),
            MirInst::Call { target, dest_hi: None, .. } => numbers.contains(target),
            MirInst::Call { target, dest_more, .. } if !dest_more.is_empty() => numbers.contains(target),
            // A hash or a slot index.
            MirInst::HostInvoke { native_id, dest_hi: None, .. } => common::host_returns_number(u64::from(*native_id)),
            // A wide lane element's words are all numbers.
            MirInst::LaneIndex { dest_more, .. } => !dest_more.is_empty(),
            MirInst::Phi { .. } => true,
            _ => false,
        })
        .flat_map(MirInst::dests)
        .collect();
    // A ground body's `int` parameter is a number, unless it is a pair's
    // low word, typed [`MirTy::MaybeObj`] (it may carry a heap payload).
    if !func.opaque_words {
        out.extend(func.params.iter().copied().filter(|p| ty_of(func, *p) == MirTy::I64));
    }
    // A word of a wide result is a number where every return of the
    // callee's gives one ([`number_return_words`]).
    for inst in insts() {
        if let MirInst::Call { target, dest_more, .. } = inst
            && !dest_more.is_empty()
            && let Some(mask) = wide.get(target)
        {
            out.extend(inst.dests().zip(mask).filter(|(_, n)| **n).map(|(d, _)| d));
        }
    }
    // A bit op of numbers is a number (a pointer's untagging is a bit op
    // of a word that may be one), optimistically as φs.
    out.extend(insts().filter(|i| matches!(i, MirInst::Bin { op: MirBinOp::BitAnd | MirBinOp::BitOr | MirBinOp::Xor, .. })).map(MirInst::dest));
    loop {
        let before = out.len();
        for inst in insts() {
            match inst {
                MirInst::Phi { dest, args, .. } if out.contains(dest) && args.iter().any(|(_, a)| !out.contains(a)) => {
                    out.remove(dest);
                }
                MirInst::Bin { dest, op: MirBinOp::BitAnd | MirBinOp::BitOr | MirBinOp::Xor, lhs, rhs, .. } if out.contains(dest) && !(out.contains(lhs) && out.contains(rhs)) => {
                    out.remove(dest);
                }
                _ => {}
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
        .filter(|v| ty_of(func, *v).is_int())
        .collect();
    loop {
        let before = set.len();
        for i in insts() {
            let ok = |v: ValueId| match i {
                MirInst::Bin { op, .. } => !matches!(op, MirBinOp::BitAnd | MirBinOp::BitOr | MirBinOp::Xor),
                MirInst::Cmp { .. } => true,
                MirInst::Index { array, .. } | MirInst::LaneIndex { array, .. } | MirInst::CowIndex { array, .. } => *array != v,
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
        MirInst::Index { index, .. } | MirInst::LaneIndex { index, .. } | MirInst::StoreIndex { index, .. } | MirInst::CowIndex { index, .. } => *index == v,
        _ => false,
    })
}

/// Every value some instruction, terminator or debug slot reads.
fn used_values(func: &MirFunc) -> HashSet<ValueId> {
    let mut used: HashSet<ValueId> = func.debug_slots.values().copied().collect();
    for b in &func.blocks {
        for i in &b.insts {
            used.extend(i.operands());
        }
        if let Some(t) = &b.term {
            used.extend(t.uses().chain(t.binds().iter().copied()));
        }
    }
    used
}

/// Whether an instruction may write a word into an object, or run code
/// that may: what ends a load's borrow of its holder ([`Pass::new`]).
fn may_write_words(inst: &MirInst, pure: &HashSet<ValueId>) -> bool {
    match inst {
        // A pure function's call writes into no object it is passed.
        MirInst::Call { dest, .. } if pure.contains(dest) => false,
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
        // A number stored, a word appended, a field or an element made
        // unique: none drops an owner's last count (a copy is made only of
        // a shared object, whose other owner keeps it).
        MirInst::HeapFieldStore { scalar: true, .. }
        | MirInst::StoreIndex { scalar: true, .. }
        | MirInst::ArrayPush { .. }
        | MirInst::Cow { field: Some(_), .. }
        | MirInst::CowIndex { .. } => false,
        MirInst::HostInvoke { native_id, .. } if common::host_writes_only_numbers(u64::from(*native_id)) || common::host_is_leaf(u64::from(*native_id)) => false,
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
fn reads_only(inst: &MirInst, v: ValueId, pure: &HashSet<ValueId>) -> bool {
    match inst {
        MirInst::ProductLoad { object, .. } => *object == v,
        MirInst::Index { array, index, .. } | MirInst::LaneIndex { array, index, .. } => *array == v && *index != v,
        MirInst::ArrayLen { .. } | MirInst::Cmp { .. } | MirInst::KeepAlive { .. } | MirInst::ClearWord { .. } | MirInst::GcBarrier { .. } => true,
        MirInst::MatchPayload { scrutinee, .. } => *scrutinee == v,
        MirInst::HostInvoke { .. } => !may_write_words(inst, pure),
        // Writes into it that keep no copy of it.
        MirInst::HeapFieldStore { scalar: true, object, value, .. } => *object == v && *value != v,
        MirInst::StoreIndex { scalar: true, array, index, .. } => *array == v && *index != v,
        MirInst::ArrayPush { array, value, .. } => *array == v && *value != v,
        MirInst::Cow { field: Some(_), src, .. } => *src == v,
        MirInst::CowIndex { array, index, .. } => *array == v && *index != v,
        _ => false,
    }
}

/// [`insert_rc_all`] of a body, or of a cleanup pad (`pad`: it runs while the
/// frame unwinds, so it keeps no homes of its own).
fn insert_rc_in(func: &mut MirFunc, pad: bool, facts: &Facts, own: Option<Label>, coro: bool) -> usize {
    let mut n = 0;
    let mut pad_homes = Vec::new();
    if let Some(p) = func.cleanup.as_deref_mut() {
        pad_homes = Pass::new(p, facts, false, false).object_homes(p);
        n += insert_rc_in(p, true, facts, None, false);
    }
    // A `FreeEarly` (`hir/early_free.rs`) stays: in a counting run the
    // runtime frees nothing there and only runs the `drop`s the value's
    // last decrement will owe, at the scope's end (plan step 7). It reads
    // its value, so the decrement comes after it.
    let pass = Pass::new(func, facts, own.is_some_and(|l| facts.numbers.contains(&l)), coro);
    let mut object_homes = pass.object_homes(func);
    object_homes.extend(pad_homes);
    object_homes.sort_unstable();
    object_homes.dedup();
    // A tail twin keeps no homes, which would end its own tail calls (an
    // unwind through it leaks what it owned, as one through a tail call
    // did before).
    let twin = own.is_some_and(|l| facts.owned_params.contains_key(&l))
        && func.blocks.iter().any(|b| b.insts.last().is_some_and(|i| is_tail_call_inst(func, b, i)));
    let (ops, homes) = pass.run(func, !pad && !twin);
    n += ops;
    if let Some(p) = func.cleanup.as_deref_mut() {
        n += release_object_homes(p, &object_homes);
    }
    n -= cancel_pairs(func, &homes);
    n + unwind_homes(func, &homes)
}

/// Before a cleanup pad resumes the unwind (its `defer`s done), the words
/// its function's `defer` homes own (`homes`) lose their owner, as at a
/// return. Returns how many ops it added.
fn release_object_homes(pad: &mut MirFunc, homes: &[u16]) -> usize {
    let all = homes;
    let mut added = 0;
    for bi in 0..pad.blocks.len() {
        let Some(at) = pad.blocks[bi].insts.iter().position(|i| matches!(i, MirInst::HostInvoke { native_id, .. } if *native_id == common::UNWIND_RESUME_ID)) else {
            continue;
        };
        let mut ops = Vec::new();
        for &h in all {
            let w = ValueId(pad.types.len() as u32);
            pad.types.push(MirTy::I64);
            let t = ValueId(pad.types.len() as u32);
            pad.types.push(MirTy::Bool);
            ops.push(MirInst::HomeLoad { dest: w, home: h });
            ops.push(MirInst::RcDec { dest: t, src: w, guard: None });
        }
        added += ops.len();
        pad.blocks[bi].insts.splice(at..at, ops);
    }
    added
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
pub(super) fn new_pad(func: &MirFunc) -> Option<MirFunc> {
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
            | MirInst::Cow { dest, unique: false, field: None, .. }
            // The field's word after the copy, which the holder owns.
            | MirInst::Cow { dest, field: Some(_), .. }
            | MirInst::CowIndex { dest, .. } => Some(*dest),
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
            // A leaf native releases nothing ([`common::HostReach::Leaf`]).
            MirInst::HostInvoke { native_id, .. } => common::host_is_leaf(u64::from(*native_id)),
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
    /// The tail twins' released parameters ([`Facts::owned_params`]).
    owned_params: HashMap<Label, Vec<bool>>,
    /// [`Facts::twin_of`].
    twin_of: HashMap<Label, Label>,
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
    fn new(func: &MirFunc, facts: &Facts, returns_number: bool, coro: bool) -> Self {
        let numbers = &facts.numbers;
        let quiet = quiet_calls(func, &facts.quiet);
        let pointers = pointer_words(func);
        let mut counted: HashSet<ValueId> = (0..func.types.len() as u32).map(ValueId).filter(|v| matches!(ty_of(func, *v), t if heap_word(t) || t == MirTy::Value)).collect();
        // A call of a body that only returns numbers hands back no object.
        let number_calls: HashSet<ValueId> = if numbers.is_empty() && facts.wide_numbers.is_empty() {
            HashSet::new()
        } else {
            number_words(func, numbers, &facts.wide_numbers)
                .into_iter()
                .filter(|v| ty_of(func, *v).is_int())
                .collect()
        };
        // An opaque body's `int` words may be a type variable's, but not
        // those a number call or a native that only returns numbers gives.
        let number_hosts: HashSet<ValueId> = func
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .filter(|i| matches!(i, MirInst::HostInvoke { native_id, .. } if common::host_returns_number(u64::from(*native_id))))
            .flat_map(MirInst::dests)
            .collect();
        counted.extend(opaque_words(func).into_iter().filter(|v| !number_calls.contains(v) && !number_hosts.contains(v)));
        counted.extend(maybe_obj_words(func));
        counted.extend(pointers);
        // An `int` word read from an object or handed back by a call may
        // be a pointer the types do not say (a pair's low word, a payload
        // of a type variable): counted, the runtime tests the address.
        let maybe = |v: ValueId| matches!(ty_of(func, v), MirTy::I64 | MirTy::MaybeObj | MirTy::Value);
        for inst in func.blocks.iter().flat_map(|b| &b.insts) {
            match inst {
                // A wide lane element's words are all numbers.
                MirInst::LaneIndex { dest_more, .. } if !dest_more.is_empty() => {}
                MirInst::MatchPayload { .. }
                | MirInst::ProductLoad { .. }
                | MirInst::Index { .. }
                | MirInst::LaneIndex { .. }
                | MirInst::LoadStatic { .. }
                | MirInst::HomeLoad { .. }
                | MirInst::Call { .. }
                | MirInst::CallIndirect { .. }
                | MirInst::ResumeCoro { .. }
                | MirInst::Yield { .. } => counted.extend(inst.dests().filter(|d| maybe(*d) && !number_calls.contains(d))),
                // A host result typed `int` may be a pointer word the
                // types do not say, unless the native only returns numbers.
                MirInst::HostInvoke { native_id, .. } if common::host_returns_number(u64::from(*native_id)) => {}
                MirInst::HostInvoke { .. } => counted.extend(inst.dests().filter(|d| maybe(*d) && !number_calls.contains(d))),
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
        // A word a number store writes is a number by its static type,
        // wherever else the frame uses it.
        let stored: HashSet<ValueId> = func
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .filter_map(|i| match i {
                MirInst::StoreIndex { value, scalar: true, .. } | MirInst::HeapFieldStore { value, scalar: true, .. } => Some(*value),
                _ => None,
            })
            .collect();
        counted.retain(|v| !stored.contains(v));
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
                        MirInst::Call { dest_hi, dest_more, .. } => {
                            owned_at_def.insert(dest);
                            for h in dest_hi.iter().chain(dest_more) {
                                if counted.contains(h) {
                                    base.insert(*h, *h);
                                    owned_at_def.insert(*h);
                                }
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
                        // A coroutine hands its resumer the word it yields
                        // or returns with an owner.
                        MirInst::ResumeCoro { .. } => {
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
        let generator = coro || super::frame_words::is_generator(func);
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
        let used = used_values(func);
        let mut changed = !borrowed.is_empty();
        while changed {
            changed = false;
            for (bi, block) in func.blocks.iter().enumerate() {
                for (ii, inst) in block.insts.iter().enumerate() {
                    let (dest, holder) = match inst {
                        MirInst::ProductLoad { dest, object, .. } => (*dest, *object),
                        MirInst::Index { dest, array, .. } | MirInst::CowIndex { dest, array, .. } => (*dest, *array),
                        _ => continue,
                    };
                    if base.get(&dest) != Some(&dest) || borrowed.contains(&dest) || !base.get(&holder).is_some_and(|r| borrowed.contains(r)) {
                        continue;
                    }
                    // An alias nothing reads (a check's barrier) keeps no copy.
                    if base.iter().any(|(k, r)| *r == dest && *k != dest && used.contains(k)) {
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
                    let ok = rest[..last].iter().all(|i| if i.operands().contains(&dest) { reads_only(i, dest, &quiet) } else { !may_write_words(i, &quiet) });
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
        let mut scalar: HashSet<ValueId> = scalar;
        // A φ of numbers only is a number (a loop's running sum).
        scalar.extend(number_words(func, numbers, &facts.wide_numbers).into_iter().filter(|d| !base.contains_key(d)));
        let mut pass = Self { base, borrowed, owned_at_def, scalar, owned_params: facts.owned_params.clone(), twin_of: facts.twin_of.clone() };
        pass.borrow_loads_of_live_holders(func, &quiet);
        pass
    }

    /// A load from a holder the frame owns and keeps past the load's last
    /// use, read only within its block and before anything may write a word
    /// into an object, is borrowed: the frame's owner of the holder keeps
    /// it that long (as a borrowed holder's caller does, in [`Self::new`]).
    fn borrow_loads_of_live_holders(&mut self, func: &MirFunc, quiet: &HashSet<ValueId>) {
        let live_in = self.live_in(func);
        let used = used_values(func);
        let mut changed = true;
        while changed {
            changed = false;
            for (bi, block) in func.blocks.iter().enumerate() {
                let b = BlockId(bi as u32);
                let mut live_after: Vec<HashSet<ValueId>> = vec![HashSet::new(); block.insts.len()];
                let mut live = self.live_end(func, b, &live_in);
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
                for (ii, inst) in block.insts.iter().enumerate() {
                    let (dest, holder) = match inst {
                        MirInst::ProductLoad { dest, object, .. } => (*dest, *object),
                        MirInst::Index { dest, array, .. } | MirInst::CowIndex { dest, array, .. } => (*dest, *array),
                        _ => continue,
                    };
                    if self.base.get(&dest) != Some(&dest) || self.borrowed.contains(&dest) {
                        continue;
                    }
                    let Some(h) = self.root(holder) else { continue };
                    if self.base.iter().any(|(k, r)| *r == dest && *k != dest && used.contains(k)) {
                        continue;
                    }
                    let elsewhere = func.blocks.iter().enumerate().any(|(bj, b)| {
                        b.term.as_ref().is_some_and(|t| t.uses().any(|u| u == dest)) || (bj != bi && b.insts.iter().any(|i| i.operands().contains(&dest)))
                    });
                    if elsewhere {
                        continue;
                    }
                    let rest = &block.insts[ii + 1..];
                    let Some(last) = rest.iter().rposition(|i| i.operands().contains(&dest)) else { continue };
                    let kept = self.borrowed.contains(&h) || live_after[ii + 1 + last].contains(&h);
                    if kept && rest[..=last].iter().all(|i| if i.operands().contains(&dest) { reads_only(i, dest, quiet) } else { !may_write_words(i, quiet) }) {
                        self.borrowed.insert(dest);
                        changed = true;
                    }
                }
            }
        }
    }

    /// The homes some `HomeStore` writes an object word to, in order.
    fn object_homes(&self, func: &MirFunc) -> Vec<u16> {
        let mut homes: Vec<u16> = func
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .filter_map(|i| match i {
                MirInst::HomeStore { home, value, .. } if self.root(*value).is_some() || self.maybe_object(func, *value) => Some(*home),
                _ => None,
            })
            .collect();
        homes.sort_unstable();
        homes.dedup();
        homes
    }

    /// Whether `v`, which is no counted word, may still name an object the
    /// types do not say (an `int` or untyped word that is no number).
    fn maybe_object(&self, func: &MirFunc, v: ValueId) -> bool {
        matches!(ty_of(func, v), MirTy::I64 | MirTy::MaybeObj | MirTy::Value) && !self.scalar.contains(&v)
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
        let storable = |r: ValueId| matches!(ty_of(func, r), MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes | MirTy::I64 | MirTy::MaybeObj);
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
                    // A tail call takes the frame, homes and all: what it
                    // passes is not released after it either (a leak).
                    if may_unwind(inst) && !is_tail_call_inst(func, block, inst) {
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
        // Calls that enter an owning twin: (block, instruction, twin).
        let mut retarget: Vec<(usize, usize, Label)> = Vec::new();
        // Per block, the frees of moved roots, which go.
        let mut strip: Vec<HashSet<usize>> = vec![HashSet::new(); n];
        // The `defer` homes that hold an object: each owns its word, from a
        // store (the old word released after it) to the next, and to the
        // frame's end, where a return (or the pad, [`release_object_homes`])
        // releases the last.
        let object_homes = self.object_homes(func);
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
            // Roots this block moves into an owning twin, past which it
            // neither releases nor frees them.
            let mut moved_here: HashSet<ValueId> = HashSet::new();
            // The uses after call `ii` of root `r`, when all of them are
            // frees (`FreeEarly`) and slot clears up to its last use in the
            // block: the call is its last real use.
            let only_frees_after = |ii: usize, r: ValueId| -> Option<Vec<usize>> {
                let mut out = Vec::new();
                for (j, i) in block.insts.iter().enumerate().skip(ii + 1) {
                    if self.used_roots(i).contains(&r) {
                        match i {
                            MirInst::FreeEarly { value, .. } | MirInst::ClearWord { value, .. } if self.root(*value) == Some(r) => out.push(j),
                            _ => return None,
                        }
                    }
                    if !live_after[j].contains(&r) {
                        return Some(out);
                    }
                }
                None
            };
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
                        let w = if ty_of(func, d).is_int() {
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
                // A tail call into a twin that owns its parameters hands
                // each one it releases an owner: a root that dies here
                // moves (once), anything else gains one.
                if let MirInst::Call { target, args, .. } = inst
                    && is_tail_call_inst(func, block, inst)
                    && let Some(mask) = self.owned_params.get(target)
                {
                    let mut moved: HashSet<ValueId> = HashSet::new();
                    for (k, &a) in args.iter().enumerate() {
                        if !mask.get(k).copied().unwrap_or(false) {
                            continue;
                        }
                        match self.owned(a) {
                            Some(r) if !live_after[ii].contains(&r) && moved.insert(r) => {}
                            _ => {
                                before[bi].entry(ii).or_default().push(MirInst::RcInc { dest: tok(&mut new_types), src: a, guard: None });
                                inserted += 1;
                            }
                        }
                    }
                }
                // A direct call to a body with an owning twin, an argument of
                // which this frame owns and passes for the last time, enters
                // the twin: that argument moves, any other it releases
                // gains an owner (as at a tail call).
                let mut moved: HashSet<ValueId> = HashSet::new();
                let dying = |r: ValueId| !live_after[ii].contains(&r) || only_frees_after(ii, r).is_some();
                if let MirInst::Call { target, args, .. } = inst
                    && !is_tail_call_inst(func, block, inst)
                    && let Some(&twin) = self.twin_of.get(target)
                    && let Some(mask) = self.owned_params.get(&twin)
                    // A call may pass fewer words than the body takes (the
                    // ones it never reads): the twin would release those.
                    && mask.iter().skip(args.len()).all(|&m| !m)
                    && args.iter().enumerate().any(|(k, &a)| mask.get(k).copied().unwrap_or(false) && self.owned(a).is_some_and(dying))
                {
                    retarget.push((bi, ii, twin));
                    for (k, &a) in args.iter().enumerate() {
                        if !mask.get(k).copied().unwrap_or(false) {
                            continue;
                        }
                        match self.owned(a) {
                            Some(r) if dying(r) && moved.insert(r) => {
                                // Its frees after the call would read what
                                // the callee owns now: they go.
                                if let Some(frees) = only_frees_after(ii, r) {
                                    strip[bi].extend(frees);
                                }
                                moved_here.insert(r);
                                // The callee owns it now: a panic in it is
                                // the callee's to release.
                                if let Some(&h) = homes.get(&r) {
                                    let zero = tok(&mut new_types);
                                    *new_types.last_mut().expect("the word's type") = MirTy::I64;
                                    let at = before[bi].entry(ii).or_default();
                                    at.push(MirInst::Const { dest: zero, c: super::inst::MirConst::I64(0) });
                                    at.push(MirInst::HomeStore { dest: tok(&mut new_types), home: h, value: zero });
                                    inserted += 2;
                                }
                            }
                            _ => {
                                before[bi].entry(ii).or_default().push(MirInst::RcInc { dest: tok(&mut new_types), src: a, guard: None });
                                inserted += 1;
                            }
                        }
                    }
                }
                if !inst.is_phi() && !is_tail_call_inst(func, block, inst) {
                    for r in self.used_roots(inst) {
                        if !live_after[ii].contains(&r) && !moved.contains(&r) && !moved_here.contains(&r) {
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
                // A static, or a `defer` home that holds an object, owns its
                // word: a store reads the old one first and releases it
                // after (a home starts at zero).
                if let MirInst::StoreStatic { value, .. } = inst
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
                // A yield hands its word to the resumer with an owner of its
                // own, as a return does.
                if let MirInst::Yield { value, from: false, .. } = inst
                    && (self.root(*value).is_some() || self.maybe_object(func, *value))
                {
                    before[bi].entry(ii).or_default().push(MirInst::RcInc { dest: tok(&mut new_types), src: *value, guard: None });
                    inserted += 1;
                }
                if let MirInst::HomeStore { home, value, .. } = inst
                    && object_homes.contains(home)
                {
                    let old = tok(&mut new_types);
                    *new_types.last_mut().expect("the token's type") = MirTy::I64;
                    before[bi].entry(ii).or_default().push(MirInst::HomeLoad { dest: old, home: *home });
                    ops.insert(0, MirInst::RcInc { dest: tok(&mut new_types), src: *value, guard: None });
                    ops.insert(1, MirInst::RcDec { dest: tok(&mut new_types), src: old, guard: None });
                }
                if !ops.is_empty() {
                    inserted += ops.len();
                    after[bi].insert(ii, ops);
                }
            }
            // The terminator.
            match &term {
                Terminator::Return { lo, hi, more } => {
                    let mut moved: HashSet<ValueId> = HashSet::new();
                    let tail = block.insts.last().is_some_and(|i| is_tail_call_inst(func, block, i));
                    for v in [*lo, *hi].into_iter().flatten().chain(more.iter().copied()) {
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
                    // So do the words the `defer` homes own.
                    for &h in &object_homes {
                        let w = tok(&mut new_types);
                        *new_types.last_mut().expect("the token's type") = MirTy::I64;
                        before_term[bi].push(MirInst::HomeLoad { dest: w, home: h });
                        before_term[bi].push(MirInst::RcDec { dest: tok(&mut new_types), src: w, guard: None });
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
                                        let w = if ty_of(func, p).is_int() {
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
        // Owned parameters (a coroutine body's) are in their homes from the
        // start: a panic before their last use releases them. One the body
        // never reads dies at the start. Not when the entry is a jump
        // target, where these ops would run again.
        let entry = func.entry;
        let entry_targeted = func.blocks.iter().any(|b| b.term.as_ref().is_some_and(|t| t.succs().contains(&entry)));
        if !entry_targeted {
            let mut ops = Vec::new();
            for &p in &func.params {
                if self.borrowed.contains(&p) || self.root(p) != Some(p) {
                    continue;
                }
                if !live_in[entry.index()].contains(&p) {
                    ops.push(MirInst::RcDec { dest: tok(&mut new_types), src: p, guard: None });
                    continue;
                }
                let Some(&h) = homes.get(&p) else { continue };
                let w = if ty_of(func, p).is_int() {
                    p
                } else {
                    let w = tok(&mut new_types);
                    *new_types.last_mut().expect("the word's type") = MirTy::I64;
                    ops.push(MirInst::Cast { dest: w, kind: MirCastKind::Word, to: MirTy::I64, src: p });
                    w
                };
                ops.push(MirInst::HomeStore { dest: tok(&mut new_types), home: h, value: w });
            }
            if !ops.is_empty() {
                inserted += ops.len();
                let ei = entry.0 as usize;
                if func.blocks[ei].insts.is_empty() {
                    ops.append(&mut before_term[ei]);
                    before_term[ei] = ops;
                } else {
                    let at = before[ei].entry(0).or_default();
                    ops.append(at);
                    *at = ops;
                }
            }
        }
        func.types.extend(new_types);
        for (bi, ii, twin) in retarget {
            if let MirInst::Call { target, .. } = &mut func.blocks[bi].insts[ii] {
                *target = twin;
            }
        }
        // Materialize: in-block code first (positions are the old ones).
        for (bi, block) in func.blocks.iter_mut().enumerate() {
            if after[bi].is_empty() && before[bi].is_empty() && before_term[bi].is_empty() && strip[bi].is_empty() {
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
                if !strip[bi].contains(&ii) {
                    block.insts.push(inst);
                }
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
/// run code that does. A leaf native does none of that
/// ([`common::HostReach::Leaf`]): what the frame owns needs no home there.
fn may_unwind(inst: &MirInst) -> bool {
    if let MirInst::HostInvoke { native_id, .. } = inst
        && common::host_is_leaf(u64::from(*native_id))
    {
        return false;
    }
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
        insert_rc_all([(Some(Label(7)), &mut f)], &HashMap::new());
        assert_eq!(all_rc_ops(&f), 0, "{f}");
        // Called under another label, its result is a word to count.
        let mut g = crate::mir::parse_func(walk).expect(walk);
        insert_rc_all([(Some(Label(8)), &mut g)], &HashMap::new());
        assert_eq!(all_rc_ops(&g), 1, "{g}");
    }

    /// An `int` field the frame only adds up is no word to count; one it
    /// hands on (here returned) is.
    #[test]
    fn an_int_field_summed_counts_nothing() {
        let sum = "func @s(v0: heapref, v1: i64) -> i64 {\nbb0:\n    v2 = productload v0, 0\n    v3 = iadd v1, v2\n    return v3\n}\n";
        let mut f = crate::mir::parse_func(sum).expect(sum);
        insert_rc_all([(None, &mut f)], &HashMap::new());
        assert_eq!(all_rc_ops(&f), 0, "{f}");
        let kept = "func @k(v0: heapref) -> i64 {\nbb0:\n    v2 = productload v0, 0\n    return v2\n}\n";
        let mut g = crate::mir::parse_func(kept).expect(kept);
        insert_rc_all([(None, &mut g)], &HashMap::new());
        assert!(all_rc_ops(&g) > 0, "{g}");
    }

    /// A field of a borrowed object handed to a host native that only
    /// reads it borrows the object's count; handed to a call, which may
    /// overwrite the field, it takes one of its own.
    #[test]
    fn a_borrowed_field_read_by_a_host_counts_nothing() {
        let read = "func @r(v0: heapref, v1: i64) -> i64 {\nbb0:\n    v2 = productload v0, 0\n    v3 = host.key_index_probe v2, v2, v1, v1\n    v4 = iadd v3, v1\n    return v4\n}\n";
        let mut f = crate::mir::parse_func(read).expect(read);
        insert_rc_all([(None, &mut f)], &HashMap::new());
        assert_eq!(all_rc_ops(&f), 0, "{f}");
        let call = "func @c(v0: heapref, v1: i64) -> i64 {\nbb0:\n    v2 = productload v0, 0\n    v3 = call.i64 7 v2\n    v4 = iadd v3, v1\n    return v4\n}\n";
        let mut g = crate::mir::parse_func(call).expect(call);
        insert_rc_all([(None, &mut g)], &HashMap::new());
        assert!(all_rc_ops(&g) > 0, "{g}");
    }

    fn homes_stored(f: &MirFunc) -> usize {
        f.blocks.iter().flat_map(|b| &b.insts).filter(|i| matches!(i, MirInst::HomeStore { .. })).count()
    }

    /// An owned word live across a leaf native's call needs no home: the
    /// call cannot unwind the frame. Across one that may (here `hash_string`,
    /// which is not marked a leaf) it is kept in a home.
    #[test]
    fn a_leaf_host_call_needs_no_home() {
        let leaf = "func @f(v1: i64) -> i64 {\nbb0:\n    v2 = call.heapref 9 v1\n    v3 = host.string_byte_at v2, v1\n    v4 = host.string_byte_at v2, v3\n    return v4\n}\n";
        let mut f = crate::mir::parse_func(leaf).expect(leaf);
        insert_rc_all([(None, &mut f)], &HashMap::new());
        assert_eq!(homes_stored(&f), 0, "{f}");
        let runtime = leaf.replace("v3 = host.string_byte_at v2, v1", "v3 = host.hash_string v2");
        let mut g = crate::mir::parse_func(&runtime).expect(&runtime);
        insert_rc_all([(None, &mut g)], &HashMap::new());
        assert!(homes_stored(&g) > 0, "{g}");
    }

    /// A field of a borrowed object stays borrowed across a leaf native
    /// that may change the holder's length (`vec_reserve`): it releases
    /// nothing, so the load takes no count of its own.
    #[test]
    fn a_borrowed_field_stays_borrowed_across_a_leaf() {
        let src = "func @r(v0: heapref, v1: i64) -> i64 {\nbb0:\n    v2 = productload v0, 0\n    v3 = host.vec_reserve v0, v1\n    v4 = host.string_byte_at v2, v1\n    v5 = iadd v4, v1\n    return v5\n}\n";
        let mut f = crate::mir::parse_func(src).expect(src);
        insert_rc_all([(None, &mut f)], &HashMap::new());
        assert_eq!(all_rc_ops(&f), 0, "{f}");
        let popped = src.replace("host.vec_reserve v0, v1", "host.vec_pop v0");
        let mut g = crate::mir::parse_func(&popped).expect(&popped);
        insert_rc_all([(None, &mut g)], &HashMap::new());
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
