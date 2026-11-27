//! What an engine that runs MIR needs to know about a body's frame words:
//! their kinds, which of them the collector must see, and how a body reads
//! its parameters. Cranelift (`coil-native`) and the MIR interpreter
//! (`coil-mir-interp`) both read these; nothing here depends on either.

use std::collections::{HashMap, HashSet};

use super::{MirCastKind, MirFunc, MirInst, MirTy, Terminator, ValueId};

/// One past the last parameter `f` reads. A `CALL` passes the words its
/// arity says, which a body's MIR parameters may outnumber (an unread
/// trait-method parameter) or fall short of: native code passes zero for
/// the unread ones and drops the extra words, as the runtime's frame ignores
/// them.
pub fn used_params(f: &MirFunc) -> usize {
    let mut used: HashSet<ValueId> = HashSet::new();
    for b in &f.blocks {
        for inst in &b.insts {
            used.extend(inst.operands());
        }
        used.extend(b.term.iter().flat_map(Terminator::uses));
    }
    f.params.iter().rposition(|p| used.contains(p)).map_or(0, |i| i + 1)
}

/// A generator's body: it yields.
pub fn is_generator(f: &MirFunc) -> bool {
    f.blocks.iter().flat_map(|b| &b.insts).any(|i| matches!(i, MirInst::Yield { .. }))
}

/// An instruction whose helper may allocate.
pub fn allocates(inst: &MirInst) -> bool {
    inst.props().allocates
}

/// A value native code holds: a scalar, or a heap word it passes along and
/// reads through (heap tier).
pub fn numeric(t: MirTy) -> bool {
    scalar(t) || heap_word(t)
}

/// A value native arithmetic works on.
pub fn scalar(t: MirTy) -> bool {
    matches!(t, MirTy::I64 | MirTy::MaybeObj | MirTy::F64 | MirTy::Bool)
}

/// The `gc::root` handles `f` makes (the results of its `gc_root` host
/// calls): each keeps its object until the function returns, where other
/// words die at their last use.
pub fn root_handles(f: &MirFunc) -> Vec<ValueId> {
    f.blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter_map(|i| match i {
            MirInst::HostInvoke { dest, native_id, .. } if *native_id == common::GC_ROOT_ID => Some(*dest),
            _ => None,
        })
        .collect()
}

/// A heap object word (a pointer, or a niche `Option` / `Result` word).
pub fn heap_word(t: MirTy) -> bool {
    matches!(t, MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes)
}

/// The `int` words of an [`MirFunc::opaque_words`] body that may hold an
/// object (a type variable's word): all but those an arithmetic, compare,
/// constant or code pointer defines.
pub fn opaque_words(f: &MirFunc) -> Vec<ValueId> {
    if !f.opaque_words {
        return Vec::new();
    }
    let scalar: HashSet<ValueId> = f
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter_map(|i| match i {
            MirInst::Const { dest, .. }
            | MirInst::CodePtr { dest, .. }
            | MirInst::Bin { dest, .. }
            | MirInst::Cmp { dest, .. }
            | MirInst::Unary { dest, .. }
            | MirInst::ArrayLen { dest, .. } => Some(*dest),
            _ => None,
        })
        .collect();
    (0..f.types.len() as u32)
        .map(ValueId)
        .filter(|v| ty_of(f, *v).is_int() && !scalar.contains(v))
        .collect()
}

pub fn ty_of(f: &MirFunc, v: ValueId) -> MirTy {
    f.types.get(v.index()).copied().unwrap_or(MirTy::Bottom)
}

/// A word's kind for an object's packed word kinds, as the runtime's emission
/// records them.
pub fn word_kind(t: MirTy) -> u8 {
    match t {
        // A maybe-object word is recorded as the `i64` it is typed as
        // elsewhere: an object's kinds are what its emission says.
        MirTy::I64 | MirTy::MaybeObj | MirTy::F64 | MirTy::Bool => common::WORD_SCALAR,
        MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes => common::WORD_POINTER,
        MirTy::Value | MirTy::Bottom => common::WORD_UNKNOWN,
    }
}

/// The words of a non-heap type that hold an object for sure: a heap
/// word moved into an `int` value (an enum payload's word,
/// `Holder::One(n)`), and the φs merging only such words.
pub fn pointer_words(f: &MirFunc) -> HashSet<ValueId> {
    let mut out = HashSet::new();
    let mut changed = true;
    while changed {
        changed = false;
        for inst in f.blocks.iter().flat_map(|b| &b.insts) {
            let (dest, ok) = match inst {
                MirInst::Cast { dest, kind: MirCastKind::Word, src, .. } => (*dest, heap_word(ty_of(f, *src)) || out.contains(src)),
                MirInst::Phi { dest, args, .. } => (*dest, !args.is_empty() && args.iter().all(|(_, a)| heap_word(ty_of(f, *a)) || out.contains(a))),
                _ => continue,
            };
            if ok && ty_of(f, dest).is_int() && out.insert(dest) {
                changed = true;
            }
        }
    }
    out
}

/// The words typed [`MirTy::MaybeObj`]: an `i64` that may hold an object
/// (a pair's low word whose payload may be one), which reference counting
/// counts with a run-time address test. The φs that merge one with other
/// words are counted with it as any `i64` φ of a counted word is
/// (`rc_insert`).
pub fn maybe_obj_words(f: &MirFunc) -> impl Iterator<Item = ValueId> + '_ {
    (0..f.types.len() as u32).map(ValueId).filter(|&v| ty_of(f, v) == MirTy::MaybeObj)
}

/// Each `GcBarrier`'s object: the dest of the allocating instruction right
/// before it in its block, as the runtime's emission pairs them. Native code
/// never collects during a call, so the barrier only names that word.
pub fn barrier_pairs(f: &MirFunc) -> HashMap<ValueId, ValueId> {
    let mut out = HashMap::new();
    for b in &f.blocks {
        let mut pending = None;
        for inst in &b.insts {
            match inst {
                MirInst::Alloc { dest, .. }
                | MirInst::AllocAt { dest, .. }
                | MirInst::ArrayPush { dest, .. }
                | MirInst::LanePush { dest, .. }
                | MirInst::LanesToWords { dest, .. }
                | MirInst::Format { dest, .. }
                | MirInst::Stringify { dest, .. }
                | MirInst::Cow { dest, .. } => pending = Some(*dest),
                MirInst::GcBarrier { dest, .. } => {
                    if let Some(obj) = pending.take() {
                        out.entry(*dest).or_insert(obj);
                    }
                }
                _ => pending = None,
            }
        }
    }
    out
}
