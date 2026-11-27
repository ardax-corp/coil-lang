//! Wide direct returns: a body that returns a fresh object of three or four
//! immediate words (an arity-3 or arity-4 tuple of scalars, or an `Option`
//! of a tuple of scalars, which is a niche word over a tuple object) gets a
//! twin returning those words in registers (`Return { lo, hi, more }`), and
//! each direct call that only reads the object's fields and, for an
//! `Option`, whether it is `Some`, calls the twin instead. Neither side then
//! allocates the object.
//!
//! The body itself stays for every other caller (function values, the
//! runtime's entries, calls that keep the object). Runs on the captured
//! bodies before counts are inserted ([`super::native_capture`]), so the
//! object's counts never appear.

use std::collections::{HashMap, HashSet};

use super::cse::dce;
use super::func::MirFunc;
use super::inst::{MirAllocKind, MirCastKind, MirConst, MirInst, MirUnaryOp, Terminator, ValueId, MAX_RET_MORE};
use super::native_capture::NativeBody;
use super::ty::MirTy;
use crate::bodies::Label;

/// The words a body returns in place of its object.
#[derive(Clone, Debug, PartialEq)]
struct Shape {
    /// The tuple's item types, in order.
    items: Vec<MirTy>,
    /// An `Option` of the tuple: a `bool` word after the items, `true` for
    /// `Some`.
    niche: bool,
}

impl Shape {
    fn words(&self) -> Vec<MirTy> {
        let mut w = self.items.clone();
        if self.niche {
            w.push(MirTy::Bool);
        }
        w
    }
}

/// What one `return` of a body returns.
enum Ret {
    /// The tuple object `alloc` with `items`, behind the cast chain `chain`.
    Tuple { items: Vec<ValueId>, chain: Vec<ValueId> },
    /// `None`: the niche word `0`.
    None,
}

fn immediate(t: MirTy) -> bool {
    matches!(t, MirTy::I64 | MirTy::MaybeObj | MirTy::F64 | MirTy::Bool)
}

/// How many times each value is read (instructions and terminators).
fn use_counts(f: &MirFunc) -> HashMap<ValueId, usize> {
    let mut n: HashMap<ValueId, usize> = HashMap::new();
    for b in &f.blocks {
        for i in &b.insts {
            for v in i.operands() {
                *n.entry(v).or_default() += 1;
            }
        }
        if let Some(t) = &b.term {
            for v in t.uses() {
                *n.entry(v).or_default() += 1;
            }
        }
    }
    n
}

/// `v` through `Word` casts to the value they reinterpret; the casts passed.
fn through_casts(f: &MirFunc, mut v: ValueId) -> (ValueId, Vec<ValueId>) {
    let mut chain = Vec::new();
    while let Some(MirInst::Cast { kind: MirCastKind::Word, src, .. }) = f.def_inst(v) {
        chain.push(v);
        v = *src;
    }
    (v, chain)
}

fn classify_ret(f: &MirFunc, v: ValueId, niche: bool) -> Option<Ret> {
    let (root, mut chain) = through_casts(f, v);
    if niche && f.const_i64(root) == Some(0) {
        return Some(Ret::None);
    }
    match f.def_inst(root)? {
        // Each item a scalar by its type and by the kind the tuple records
        // for it: an `i64` that may be a pointer stays in its object.
        MirInst::Alloc { kind, elems, kinds, .. }
            if *kind == MirAllocKind::TUPLE
                && elems.len() < common::PACKED_KIND_WORDS
                && elems.iter().enumerate().all(|(i, e)| immediate(f.ty(*e)) && common::packed_word_kind(*kinds, i) == common::WORD_SCALAR) =>
        {
            chain.push(root);
            Some(Ret::Tuple { items: elems.clone(), chain })
        }
        _ => None,
    }
}

/// The words `f` can return instead of its object, if every return builds
/// a fresh tuple of immediates read by nothing but that return (or, for an
/// `Option`, returns `None`), and they come to three or four words.
fn shape_of(f: &MirFunc) -> Option<Shape> {
    if f.ret_hi_ty.is_some() || !f.ret_more_tys.is_empty() || f.cleanup.is_some() || super::frame_words::is_generator(f) || f.debug_keep || f.keep_lines || f.opaque_words {
        return None;
    }
    let niche = match f.ret_ty? {
        MirTy::HeapRef => false,
        MirTy::NicheOpt => true,
        _ => return None,
    };
    let uses = use_counts(f);
    let mut items: Option<Vec<MirTy>> = None;
    let mut any = false;
    for b in &f.blocks {
        let Some(Terminator::Return { lo, hi, more }) = &b.term else { continue };
        if hi.is_some() || !more.is_empty() {
            return None;
        }
        let v = (*lo)?;
        match classify_ret(f, v, niche)? {
            Ret::None => {}
            Ret::Tuple { items: elems, chain } => {
                // The object and each cast of it are read once: by the next
                // cast, or by the return.
                if chain.iter().any(|c| uses.get(c).copied().unwrap_or(0) != 1) {
                    return None;
                }
                let tys: Vec<MirTy> = elems.iter().map(|e| f.ty(*e)).collect();
                match &items {
                    Some(prev) if *prev != tys => return None,
                    _ => items = Some(tys),
                }
                any = true;
            }
        }
    }
    let items = items.filter(|_| any)?;
    let shape = Shape { items, niche };
    let n = shape.words().len();
    (3..=2 + MAX_RET_MORE).contains(&n).then_some(shape)
}

fn new_value(f: &mut MirFunc, ty: MirTy) -> ValueId {
    let v = ValueId(f.types.len() as u32);
    f.types.push(ty);
    v
}

fn zero(ty: MirTy) -> MirConst {
    match ty {
        MirTy::F64 => MirConst::F64(0),
        MirTy::Bool => MirConst::Bool(false),
        _ => MirConst::I64(0),
    }
}

/// `f` rewritten to return `shape`'s words.
fn widen_body(f: &mut MirFunc, shape: &Shape) {
    let words = shape.words();
    for bi in 0..f.blocks.len() {
        let Some(Terminator::Return { lo: Some(v), .. }) = f.blocks[bi].term.clone() else { continue };
        let ret = classify_ret(f, v, shape.niche).expect("a return the shape admitted");
        let mut out: Vec<ValueId> = Vec::with_capacity(words.len());
        match ret {
            Ret::Tuple { items, chain } => {
                // The object and its casts were read only by this return.
                let gone: HashSet<ValueId> = chain.into_iter().collect();
                for b in &mut f.blocks {
                    b.insts.retain(|i| !gone.contains(&i.dest()));
                }
                out.extend(items);
                if shape.niche {
                    let t = new_value(f, MirTy::Bool);
                    f.blocks[bi].insts.push(MirInst::Const { dest: t, c: MirConst::Bool(true) });
                    out.push(t);
                }
            }
            Ret::None => {
                for &ty in &words {
                    let z = new_value(f, ty);
                    f.blocks[bi].insts.push(MirInst::Const { dest: z, c: zero(ty) });
                    out.push(z);
                }
            }
        }
        f.blocks[bi].term = Some(Terminator::Return { lo: Some(out[0]), hi: Some(out[1]), more: out[2..].to_vec() });
    }
    f.ret_ty = Some(words[0]);
    f.ret_hi_ty = Some(words[1]);
    f.ret_more_tys = words[2..].to_vec();
    f.ret_layout = crate::layout::MirLayout::TwoSlot;
    dce(f);
}

/// A call's result `d` as the reads [`widen_call`] replaces: the casts of
/// it, the field reads (`ProductLoad` of slot `i < items`) and, for an
/// `Option`, its `Some` tests (`Unary Not`). `None` when anything else
/// reads it.
struct Reads {
    casts: HashSet<ValueId>,
    loads: Vec<(ValueId, usize)>,
    tests: Vec<ValueId>,
}

fn reads_of(f: &MirFunc, d: ValueId, shape: &Shape) -> Option<Reads> {
    let mut set: HashSet<ValueId> = HashSet::from([d]);
    let mut casts = HashSet::new();
    // Casts first, to a fixed point (a cast of a cast).
    loop {
        let mut grew = false;
        for i in f.blocks.iter().flat_map(|b| &b.insts) {
            if let MirInst::Cast { kind: MirCastKind::Word, dest, src, .. } = i
                && set.contains(src)
                && set.insert(*dest)
            {
                casts.insert(*dest);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    let mut loads = Vec::new();
    let mut tests = Vec::new();
    for b in &f.blocks {
        if b.term.as_ref().is_some_and(|t| t.uses().any(|u| set.contains(&u))) {
            return None;
        }
        for i in &b.insts {
            if !i.operands().iter().any(|o| set.contains(o)) {
                continue;
            }
            match i {
                MirInst::Cast { kind: MirCastKind::Word, dest, .. } if casts.contains(dest) => {}
                MirInst::ProductLoad { dest, slot, .. } if (*slot as usize) < shape.items.len() && f.ty(*dest) == shape.items[*slot as usize] => loads.push((*dest, *slot as usize)),
                MirInst::Unary { op: MirUnaryOp::Not, dest, .. } if shape.niche && f.ty(*dest) == MirTy::Bool => tests.push(*dest),
                _ => return None,
            }
        }
    }
    Some(Reads { casts, loads, tests })
}

/// The call whose result is `dest` retargeted to `twin`: its words replace
/// the field reads and `Some` tests of its result.
fn widen_call(f: &mut MirFunc, dest: ValueId, twin: Label, shape: &Shape, reads: Reads) {
    let words = shape.words();
    let Some((bi, ii)) = f.blocks.iter().enumerate().find_map(|(bi, b)| b.insts.iter().position(|i| matches!(i, MirInst::Call { dest: d, .. } if *d == dest)).map(|ii| (bi, ii))) else {
        return;
    };
    f.types[dest.index()] = words[0];
    let hi = new_value(f, words[1]);
    let more: Vec<ValueId> = words[2..].iter().map(|&t| new_value(f, t)).collect();
    let all: Vec<ValueId> = std::iter::once(dest).chain(std::iter::once(hi)).chain(more.iter().copied()).collect();
    if let MirInst::Call { target, dest_hi, dest_more, .. } = &mut f.blocks[bi].insts[ii] {
        *target = twin;
        *dest_hi = Some(hi);
        *dest_more = more;
    }
    let flag = shape.niche.then(|| all[shape.items.len()]);
    let mut subst: HashMap<ValueId, ValueId> = HashMap::new();
    for (load, i) in &reads.loads {
        subst.insert(*load, all[*i]);
    }
    let tests: HashSet<ValueId> = reads.tests.iter().copied().collect();
    let loads: HashSet<ValueId> = reads.loads.iter().map(|(d, _)| *d).collect();
    for b in &mut f.blocks {
        b.insts.retain(|i| !loads.contains(&i.dest()) && !matches!(i, MirInst::Cast { dest, .. } if reads.casts.contains(dest)));
        for i in &mut b.insts {
            if let MirInst::Unary { dest, src, .. } = i
                && tests.contains(dest)
            {
                *src = flag.expect("a Some test of a niche result");
            }
        }
    }
    f.replace_uses(&subst);
}

/// Give each body [`shape_of`] admits a `$words` twin when a direct call
/// can use it, and point those calls at it. Returns how many calls moved.
pub fn wide_return_twins(bodies: &mut Vec<NativeBody>) -> usize {
    let shapes: HashMap<Label, Shape> = bodies.iter().filter_map(|b| Some((b.entry?, shape_of(&b.mir)?))).collect();
    if shapes.is_empty() {
        return 0;
    }
    // Which calls can take the twin.
    let mut sites: Vec<(usize, ValueId, Label)> = Vec::new();
    for (k, b) in bodies.iter().enumerate() {
        for inst in b.mir.blocks.iter().flat_map(|bl| &bl.insts) {
            if let MirInst::Call { dest, dest_hi: None, target, .. } = inst
                && let Some(shape) = shapes.get(target)
                && reads_of(&b.mir, *dest, shape).is_some()
            {
                sites.push((k, *dest, *target));
            }
        }
    }
    if sites.is_empty() {
        return 0;
    }
    let first = bodies.iter().flat_map(|b| b.entry.into_iter().chain(b.mir.code_targets())).map(|l| l.0).max().unwrap_or(0) + 1;
    let by_label: HashMap<Label, usize> = bodies.iter().enumerate().filter_map(|(i, b)| Some((b.entry?, i))).collect();
    let mut targets: Vec<Label> = sites.iter().map(|s| s.2).collect();
    targets.sort_unstable_by_key(|l| l.0);
    targets.dedup();
    let mut twin_of: HashMap<Label, Label> = HashMap::new();
    for (next, t) in (first..).zip(&targets) {
        let src = &bodies[by_label[t]];
        let mut mir = src.mir.clone();
        widen_body(&mut mir, &shapes[t]);
        let name = format!("{}$words", src.name);
        mir.name = name.clone();
        twin_of.insert(*t, Label(next));
        bodies.push(NativeBody { name, entry: Some(Label(next)), mir });
    }
    let moved = sites.len();
    for (k, dest, t) in sites {
        let f = &mut bodies[k].mir;
        // Read again: an earlier rewrite in this body removed other reads.
        let Some(reads) = reads_of(f, dest, &shapes[&t]) else { continue };
        widen_call(f, dest, twin_of[&t], &shapes[&t], reads);
    }
    moved
}
