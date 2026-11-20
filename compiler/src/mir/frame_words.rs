//! What an engine that runs MIR needs to know about a body's frame words:
//! their kinds, which of them the collector must see, and how a body reads
//! its parameters. Cranelift (`coil-native`) and the MIR interpreter
//! (`coil-mir-interp`) both read these; nothing here depends on either.

use std::collections::{HashMap, HashSet};

use super::{BlockId, MirCastKind, MirFunc, MirInst, MirTy, Terminator, ValueId};

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
    matches!(t, MirTy::I64 | MirTy::F64 | MirTy::Bool)
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
        .filter(|v| ty_of(f, *v) == MirTy::I64 && !scalar.contains(v))
        .collect()
}

pub fn ty_of(f: &MirFunc, v: ValueId) -> MirTy {
    f.types.get(v.index()).copied().unwrap_or(MirTy::Bottom)
}

/// A word's kind for an object's packed word kinds, as the runtime's emission
/// records them.
pub fn word_kind(t: MirTy) -> u8 {
    match t {
        MirTy::I64 | MirTy::F64 | MirTy::Bool => common::WORD_SCALAR,
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
            if ok && !heap_word(ty_of(f, dest)) && ty_of(f, dest) == MirTy::I64 && out.insert(dest) {
                changed = true;
            }
        }
    }
    out
}

/// The `int` words that may hold an object with no static type to say so:
/// the low word of a pair that may carry a heap payload
/// ([`MirFunc::heap_pair_words`]), the φs and casts that copy one, and a φ
/// that merges an object with a number (an inlined `Result<int, string>`'s
/// low word: `Ok`'s `int`, `Err`'s string). An engine roots them ambiguously
/// (a pair of numbers reads the same). `pointers` are [`pointer_words`],
/// which are precise roots already.
pub fn pair_words(f: &MirFunc, pointers: &HashSet<ValueId>) -> Vec<ValueId> {
    let mut out: HashSet<ValueId> = f.heap_pair_words.iter().copied().filter(|&v| !heap_word(ty_of(f, v))).collect();
    let object = |v: &ValueId, out: &HashSet<ValueId>| out.contains(v) || pointers.contains(v) || heap_word(ty_of(f, *v));
    let mut changed = true;
    while changed {
        changed = false;
        for inst in f.blocks.iter().flat_map(|b| &b.insts) {
            let copies = match inst {
                MirInst::Phi { dest, args, .. } => ty_of(f, *dest) == MirTy::I64 && args.iter().any(|(_, a)| object(a, &out)),
                MirInst::Cast { dest, src, .. } => ty_of(f, *dest) == MirTy::I64 && out.contains(src),
                _ => false,
            };
            if copies && out.insert(inst.dest()) {
                changed = true;
            }
        }
    }
    let mut words: Vec<ValueId> = out.into_iter().filter(|v| !pointers.contains(v)).collect();
    words.sort_unstable_by_key(|v| v.0);
    words
}

/// `held[block][inst]`: the heap words locals hold just before that inst
/// (one past the last: at the terminator), by block position. As in the
/// runtime's frame, a local holds the word it was bound to
/// ([`MirFunc::local_binds`]) from the word's definition until another of
/// the same local's is defined or a `ClearWord` drops it, even past its
/// last use; a match binding holds its word only through its scrutinee.
/// Where paths that
/// left a local different words meet with no φ to merge them (the φ was
/// dead), it holds neither: what is live stays rooted anyway.
pub fn held_words(f: &MirFunc) -> Vec<Vec<HashSet<ValueId>>> {
    let mut locals: HashMap<ValueId, Vec<u32>> = HashMap::new();
    for &(l, v) in &f.local_binds {
        locals.entry(v).or_default().push(l.0);
    }
    let mut held: Vec<Vec<HashSet<ValueId>>> = f.blocks.iter().map(|b| vec![HashSet::new(); b.insts.len() + 1]).collect();
    if locals.is_empty() {
        return held;
    }
    let pos: HashMap<BlockId, usize> = f.blocks.iter().enumerate().map(|(i, b)| (b.id, i)).collect();
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); f.blocks.len()];
    for (i, b) in f.blocks.iter().enumerate() {
        for s in b.term.as_ref().map(|t| t.succs()).unwrap_or_default() {
            if let Some(&j) = pos.get(&s) {
                preds[j].push(i);
            }
        }
    }
    // Local -> the word it holds; `None`: not reached yet.
    type State = HashMap<u32, ValueId>;
    let bind = |st: &mut State, v: ValueId| {
        for &l in locals.get(&v).into_iter().flatten() {
            st.insert(l, v);
        }
    };
    // A match binding's word is its scrutinee's payload: the scrutinee
    // holds it, and once that is rebound, the binding holds nothing.
    let payloads: HashSet<ValueId> = f
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter_map(|i| match i {
            MirInst::MatchPayload { dest, .. } | MirInst::Cast { dest, .. } => Some(*dest),
            _ => None,
        })
        .collect();
    let words = |st: &State| -> HashSet<ValueId> { st.values().copied().filter(|&v| heap_word(ty_of(f, v)) && !payloads.contains(&v)).collect() };
    let entry = pos.get(&f.entry).copied();
    let mut out: Vec<Option<State>> = vec![None; f.blocks.len()];
    let mut changed = true;
    while changed {
        changed = false;
        for (bi, b) in f.blocks.iter().enumerate() {
            let mut reached = preds[bi].iter().filter_map(|&p| out[p].as_ref());
            let mut st: State = match reached.next() {
                Some(first) => {
                    let mut st = first.clone();
                    for o in reached {
                        st.retain(|l, v| o.get(l) == Some(v));
                    }
                    st
                }
                None if Some(bi) == entry => State::new(),
                None => continue,
            };
            if Some(bi) == entry {
                for &p in &f.params {
                    bind(&mut st, p);
                }
            }
            for (ii, inst) in b.insts.iter().enumerate() {
                held[bi][ii] = words(&st);
                // A local rebound to a word that holds no object.
                if let MirInst::ClearWord { value, .. } = inst {
                    st.retain(|_, v| v != value);
                }
                for d in inst.dests() {
                    bind(&mut st, d);
                }
            }
            held[bi][b.insts.len()] = words(&st);
            if out[bi].as_ref() != Some(&st) {
                out[bi] = Some(st);
                changed = true;
            }
        }
    }
    held
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
