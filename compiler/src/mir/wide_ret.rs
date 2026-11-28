//! Wide direct calls: a small object of words (a tuple, an `Option` of a
//! tuple, which is a niche word over the tuple object, a `Result` of a
//! tuple whose `Err` is a heap word, also a niche word, or any nesting of
//! those) crosses a direct call as its words when neither side needs the
//! object, up to four words once flattened.
//!
//! A body gets a twin, `name$words`, that takes such a parameter as its
//! words (one MIR parameter per word) and returns such a result as its
//! words (`Return { lo, hi, more }`), and each direct call that can hand
//! over and take the words calls the twin instead. So neither side builds
//! the object: not the caller's argument, not the callee's result.
//!
//! A value's words are its *leaves*, by one rule at any depth: an item of
//! a tuple is a leaf word (a number, a heap word) or, when it is itself a
//! tuple the pass takes apart, that tuple's words in place; an `Option`
//! of a tuple adds a `bool` word, `true` for `Some`, after its tuple's; a
//! `Result` too (`true` for `Ok`), and its `Err` payload shares one of the
//! `Ok` tuple's leaf words (`Lay::res`).
//! So `((int, int), int)`, `(string, int, int)` and
//! `Option<((int, int), string)>` are three, three and four words. The
//! only bound is the word budget (four); a tuple whose nested words do not
//! fit keeps its inner tuples as heap words.
//!
//! What "can" means is one question per value of a body (a *node*: a
//! value with its `Word` casts, safepoint barriers and copy-on-write
//! shares): whether its words are known where it is made (a fresh
//! `alloc.tuple`, a `None`, a twin's parameter, a switched call's result,
//! a φ of such, an item load of a node whose words are known) and every
//! read of it takes words (an item load, a `Some` test, an item of a
//! tuple taken apart, an argument the twin takes as words, the twin's
//! return, an early free). The answers depend on each other across calls,
//! so they are a greatest fixpoint over the whole program: start from
//! "every node dissolves", drop what breaks, repeat.
//!
//! The body itself stays for every other caller (function values,
//! dictionaries, the runtime's entries, calls that cannot pass words).
//! Runs on the captured bodies before counts are inserted
//! ([`super::native_capture`]), so the object's counts never appear: a
//! heap word among the words is counted as any other word of its type (a
//! parameter borrowed, a call's result owned).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::cse::dce;
use super::func::MirFunc;
use super::inst::{BlockId, MirAllocKind, MirBinOp, MirCastKind, MirConst, MirInst, MirUnaryOp, Terminator, ValueId, MAX_RET_MORE};
use super::native_capture::NativeBody;
use super::ty::MirTy;
use crate::bodies::Label;

/// The most words a value crosses a call in.
const MAX_WORDS: usize = 2 + MAX_RET_MORE;

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

/// `v` through `Word` casts to the value they reinterpret.
fn through_casts(f: &MirFunc, mut v: ValueId) -> ValueId {
    while let Some(MirInst::Cast { kind: MirCastKind::Word, src, .. }) = f.def_inst(v) {
        v = *src;
    }
    v
}

/// The word type two items in one place agree on: `int` words join (one
/// that may hold an object is counted), others must be the same.
fn join_word(a: MirTy, b: MirTy) -> Option<MirTy> {
    if a.is_int() && b.is_int() {
        Some(if a == MirTy::I64 && b == MirTy::I64 { MirTy::I64 } else { MirTy::MaybeObj })
    } else {
        (a == b).then_some(a)
    }
}

/// The word two values that share a place in a sum's words (an `Ok`'s
/// and an `Err`'s, `Lay::err`) are carried in: their own type when they
/// agree, an `int` word for a `float` or `bool` beside an `int` (the same
/// bits, as a pair's payload), a maybe-object word for a heap word beside
/// a number or another kind of heap word.
fn union_word(a: MirTy, b: MirTy) -> Option<MirTy> {
    if let Some(j) = join_word(a, b) {
        return Some(j);
    }
    let num = |t: MirTy| matches!(t, MirTy::I64 | MirTy::F64 | MirTy::Bool);
    match (a, b) {
        (a, b) if num(a) && num(b) && (a == MirTy::I64 || b == MirTy::I64) => Some(MirTy::I64),
        (a, b) if (a.is_heap_word() || a.is_int()) && (b.is_heap_word() || b.is_int()) => Some(MirTy::MaybeObj),
        _ => None,
    }
}

/// Whether a value of type `t` goes in (or comes out of) a word of type
/// `word`: as it is, or by a `Word` cast (a heap word in a maybe-object
/// word a `Result` shares, `Lay::res`).
fn fits(word: MirTy, t: MirTy) -> bool {
    join_word(word, t).is_some() || union_word(word, t) == Some(word)
}

/// `v` as a value of type `to`, by a `Word` cast pushed to `out` when its
/// type differs (an `int` word and a maybe-object word are one).
fn as_word(f: &mut MirFunc, v: ValueId, to: MirTy, out: &mut Vec<MirInst>) -> ValueId {
    let from = f.ty(v);
    if from.word_class() == to.word_class() {
        return v;
    }
    let d = new_value(f, to);
    out.push(MirInst::Cast { dest: d, kind: MirCastKind::Word, to, src: v });
    d
}

/// An item's word type by its kind: an `int` item the tuple's kinds do
/// not call a scalar may hold an object.
fn item_ty(t: MirTy, kind: u8) -> MirTy {
    match t {
        t if t.is_int() && kind == common::WORD_SCALAR => MirTy::I64,
        t if t.is_int() => MirTy::MaybeObj,
        t => t,
    }
}

/// Where a node's object comes from.
#[derive(Clone, Debug)]
enum Src {
    /// A fresh tuple of words.
    Alloc { items: Vec<ValueId>, kind: MirAllocKind, kinds: common::PackedKinds },
    /// Parameter `j` of the body.
    Param(usize),
    /// The result of call `c` (an index into [`Body::calls`]).
    Call(usize),
    /// A φ of other nodes' objects: its incoming values, by block.
    Phi(Vec<(BlockId, ValueId)>),
    /// Item `slot` of node `parent`, an object word.
    Item { parent: usize, slot: usize },
    /// `None`: the niche word `0`.
    None,
    /// `Err(payload)`: a `Result` niche word, the payload's address with
    /// bit 0 set.
    Err { payload: ValueId },
    /// The `Err` payload of node `parent` (its niche word with bit 0
    /// cleared), an object word.
    ErrOf { parent: usize },
    /// Anything else (another kind of tuple, a tuple of boxed values).
    Opaque,
}

/// A read of a node's object.
#[derive(Clone, Debug)]
enum Use {
    /// `productload` of item `slot` into `dest`.
    Load { dest: ValueId, slot: usize },
    /// `lnot`: whether an `Option` is `None`.
    Test,
    /// Argument `j` of call `c`.
    Arg { call: usize, j: usize },
    /// The body's `return`.
    Ret,
    /// An incoming value of the φ of node `n`.
    Phi(usize),
    /// Item `slot` of node `parent`'s fresh tuple.
    Within { parent: usize, slot: usize },
    /// An early free or a frame word's clear (its unit token `dest`):
    /// nothing to free once there is no object. An early free still holds
    /// the words that may be objects to its place (`keep`), as the object
    /// held them: a `drop` they owe runs no earlier.
    Drop { dest: ValueId, keep: bool },
    /// `iand` of a `Result`'s niche word with `1` into `dest`: whether it
    /// is an `Err`.
    ErrBit { dest: ValueId },
    /// `ixor` of a `Result`'s niche word with `1` into `dest`: its `Err`
    /// payload.
    ErrPayload { dest: ValueId },
    /// The payload of node `parent`, an `Err`.
    ErrIn { parent: usize },
}

#[derive(Clone, Debug)]
struct Node {
    /// The value its casts and barriers start from, then those.
    members: Vec<ValueId>,
    src: Src,
    uses: Vec<Use>,
    /// Some read is none of [`Use`].
    other: bool,
    /// Some member is an `Option`'s niche word.
    niche: bool,
    /// Some member is a `Result`'s niche word.
    res: bool,
}

#[derive(Clone, Debug)]
struct CallSite {
    dest: ValueId,
    /// The callee's body, when it is one of the captured bodies.
    callee: Option<usize>,
    args: Vec<ValueId>,
    /// The call reads its callee's one-word result (no `dest_hi`).
    one_word: bool,
}

/// How a `return` of a body leaves.
#[derive(Clone, Copy, Debug)]
enum RetSite {
    /// Node `n`'s object.
    Node(usize),
    /// `None`: the niche word `0`.
    None,
    /// Anything else.
    Other,
}

/// One captured body, as the fixpoint reads it.
struct Body {
    nodes: Vec<Node>,
    member: HashMap<ValueId, usize>,
    calls: Vec<CallSite>,
    rets: Vec<(usize, RetSite)>,
    params: Vec<Option<usize>>,
    /// The body may have a twin.
    twinnable: bool,
    /// Its one-word result is a heap word an object of words can stand for.
    wide_ret: bool,
    /// Per node: it, or a node a φ joins or a tuple nests it with, meets a
    /// call edge (a parameter, a call's result or argument, a return) or a
    /// φ (a value a branch picks or a loop carries). Others are
    /// `forward_tuple_loads`' to take apart, not this pass's.
    wanted: Vec<bool>,
}

fn twinnable(name: &str, f: &MirFunc) -> bool {
    !(f.cleanup.is_some()
        || super::frame_words::is_generator(f)
        || f.debug_keep
        || f.keep_lines
        || f.opaque_words
        || f.emit_refusal.is_some()
        || name.ends_with("::drop"))
}

fn object_word(t: MirTy) -> bool {
    matches!(t, MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes)
}

fn read_body(name: &str, f: &MirFunc, by_label: &HashMap<Label, usize>) -> Body {
    let twin = twinnable(name, f);
    let defs = f.defs();
    // `v` is `0` through `Word` casts: a `None`.
    let is_zero = |mut v: ValueId| loop {
        match defs.get(&v).map(|(_, i)| *i) {
            Some(MirInst::Cast { kind: MirCastKind::Word, src, .. }) => v = *src,
            Some(MirInst::Const { c: MirConst::I64(0), .. }) => return true,
            _ => return false,
        }
    };
    let is_one = |v: ValueId| matches!(defs.get(&v).map(|(_, i)| *i), Some(MirInst::Const { c: MirConst::I64(1), .. }));
    // `a op 1` (either way round): `a`.
    let with_one = |lhs: ValueId, rhs: ValueId| {
        if is_one(rhs) {
            Some(lhs)
        } else if is_one(lhs) {
            Some(rhs)
        } else {
            None
        }
    };
    let used: HashSet<ValueId> = f.blocks.iter().flat_map(|b| b.insts.iter().flat_map(|i| i.operands()).chain(b.term.iter().flat_map(|t| t.uses().collect::<Vec<_>>()))).collect();
    let mut nodes: Vec<Node> = Vec::new();
    let mut member: HashMap<ValueId, usize> = HashMap::new();
    let mut calls: Vec<CallSite> = Vec::new();
    let mut call_of: HashMap<ValueId, usize> = HashMap::new();
    let mut params = vec![None; f.params.len()];
    let add = |root: ValueId, src: Src, nodes: &mut Vec<Node>, member: &mut HashMap<ValueId, usize>| {
        member.insert(root, nodes.len());
        nodes.push(Node { members: vec![root], src, uses: Vec::new(), other: false, niche: f.ty(root) == MirTy::NicheOpt, res: f.ty(root) == MirTy::NicheRes });
    };
    for (j, &p) in f.params.iter().enumerate() {
        if object_word(f.ty(p)) {
            params[j] = Some(nodes.len());
            add(p, Src::Param(j), &mut nodes, &mut member);
        }
    }
    for b in &f.blocks {
        for i in &b.insts {
            match i {
                MirInst::Alloc { dest, kind, elems, kinds } if *kind == MirAllocKind::TUPLE => {
                    let words = elems.len() < common::PACKED_KIND_WORDS && elems.iter().all(|e| f.ty(*e).is_word_lane());
                    let src = if words { Src::Alloc { items: elems.clone(), kind: *kind, kinds: *kinds } } else { Src::Opaque };
                    add(*dest, src, &mut nodes, &mut member);
                }
                MirInst::Phi { dest, ty, args } if object_word(*ty) => {
                    add(*dest, Src::Phi(args.clone()), &mut nodes, &mut member);
                }
                MirInst::Call { dest, dest_hi, target, args, .. } => {
                    call_of.insert(*dest, calls.len());
                    calls.push(CallSite { dest: *dest, callee: by_label.get(target).copied(), args: args.clone(), one_word: dest_hi.is_none() });
                    if dest_hi.is_none() && object_word(f.ty(*dest)) {
                        add(*dest, Src::Call(calls.len() - 1), &mut nodes, &mut member);
                    }
                }
                MirInst::Cast { kind: MirCastKind::Word, dest, src, .. } if f.ty(*dest) == MirTy::NicheOpt && is_zero(*src) => {
                    add(*dest, Src::None, &mut nodes, &mut member);
                }
                MirInst::Bin { dest, op: MirBinOp::BitOr, lhs, rhs, .. } if f.ty(*dest) == MirTy::NicheRes => {
                    if let Some(payload) = with_one(*lhs, *rhs).filter(|p| f.ty(*p).is_heap_word()) {
                        add(*dest, Src::Err { payload }, &mut nodes, &mut member);
                    }
                }
                _ => {}
            }
        }
    }
    // Casts and barriers join their source's node, and an object word an
    // item load reads from a node is a node of its own, to a fixed point
    // (a cast of a cast, an item of an item).
    loop {
        let mut grew = false;
        for i in f.blocks.iter().flat_map(|b| &b.insts) {
            let (dest, src) = match i {
                MirInst::Cast { kind: MirCastKind::Word, dest, src, .. } => (*dest, *src),
                MirInst::GcBarrier { dest, object, .. } => (*dest, *object),
                // A share of an object nothing writes (a tuple) is its word.
                MirInst::Cow { dest, src, unique: false, field: None } => (*dest, *src),
                MirInst::ProductLoad { dest, object, slot } if object_word(f.ty(*dest)) && !member.contains_key(dest) => {
                    if let Some(&parent) = member.get(object) {
                        add(*dest, Src::Item { parent, slot: *slot as usize }, &mut nodes, &mut member);
                        grew = true;
                    }
                    continue;
                }
                MirInst::Bin { dest, op: MirBinOp::Xor, lhs, rhs, .. } if !member.contains_key(dest) => {
                    if let Some(parent) = with_one(*lhs, *rhs).and_then(|a| member.get(&a).copied())
                        && nodes[parent].res
                    {
                        add(*dest, Src::ErrOf { parent }, &mut nodes, &mut member);
                        grew = true;
                    }
                    continue;
                }
                _ => continue,
            };
            if let Some(&n) = member.get(&src)
                && !member.contains_key(&dest)
            {
                member.insert(dest, n);
                nodes[n].members.push(dest);
                nodes[n].niche |= f.ty(dest) == MirTy::NicheOpt;
                nodes[n].res |= f.ty(dest) == MirTy::NicheRes;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    for b in &f.blocks {
        for i in &b.insts {
            match i {
                MirInst::Cast { kind: MirCastKind::Word, dest, src, .. } if member.contains_key(src) && member.get(dest) == member.get(src) => continue,
                MirInst::GcBarrier { dest, object, .. } if member.contains_key(object) && member.get(dest) == member.get(object) => continue,
                MirInst::Cow { dest, src, unique: false, field: None } if member.contains_key(src) && member.get(dest) == member.get(src) => continue,
                MirInst::Cast { dest, .. } if member.get(dest).is_some_and(|&n| matches!(nodes[n].src, Src::None) && nodes[n].members[0] == *dest) => continue,
                MirInst::Bin { dest, .. } if member.get(dest).is_some_and(|&n| matches!(nodes[n].src, Src::Err { .. }) && nodes[n].members[0] == *dest) => {
                    let parent = member[dest];
                    if let Src::Err { payload } = nodes[parent].src
                        && let Some(&n) = member.get(&payload)
                    {
                        nodes[n].uses.push(Use::ErrIn { parent });
                    }
                    continue;
                }
                MirInst::Bin { dest, op: MirBinOp::Xor, lhs, rhs, .. } if member.get(dest).is_some_and(|&n| matches!(nodes[n].src, Src::ErrOf { .. }) && nodes[n].members[0] == *dest) => {
                    let a = with_one(*lhs, *rhs).expect("an Err payload's word");
                    nodes[member[&a]].uses.push(Use::ErrPayload { dest: *dest });
                    continue;
                }
                MirInst::Bin { dest, op: MirBinOp::BitAnd, lhs, rhs, .. } if with_one(*lhs, *rhs).is_some_and(|a| member.get(&a).is_some_and(|&n| nodes[n].res)) => {
                    let a = with_one(*lhs, *rhs).expect("a Result's word");
                    nodes[member[&a]].uses.push(Use::ErrBit { dest: *dest });
                    continue;
                }
                MirInst::ProductLoad { dest, object, slot } if member.contains_key(object) => {
                    nodes[member[object]].uses.push(Use::Load { dest: *dest, slot: *slot as usize });
                    continue;
                }
                MirInst::Alloc { dest, elems, .. } if member.get(dest).is_some_and(|&n| matches!(nodes[n].src, Src::Alloc { .. })) => {
                    let parent = member[dest];
                    for (slot, e) in elems.iter().enumerate() {
                        if let Some(&n) = member.get(e) {
                            nodes[n].uses.push(Use::Within { parent, slot });
                        }
                    }
                    continue;
                }
                MirInst::FreeEarly { dest, value, .. } | MirInst::ClearWord { dest, value } if member.contains_key(value) && !used.contains(dest) => {
                    let keep = matches!(i, MirInst::FreeEarly { .. });
                    nodes[member[value]].uses.push(Use::Drop { dest: *dest, keep });
                    continue;
                }
                MirInst::Phi { dest, args, .. } if member.contains_key(dest) => {
                    let p = member[dest];
                    for (_, a) in args {
                        if let Some(&n) = member.get(a) {
                            nodes[n].uses.push(Use::Phi(p));
                        } else {
                            nodes[p].src = Src::Opaque;
                        }
                    }
                    continue;
                }
                MirInst::Unary { op: MirUnaryOp::Not, dest, src } if member.contains_key(src) && f.ty(*dest) == MirTy::Bool => {
                    nodes[member[src]].uses.push(Use::Test);
                    continue;
                }
                MirInst::Call { dest, args, .. } => {
                    let c = call_of[dest];
                    for (j, a) in args.iter().enumerate() {
                        if let Some(&n) = member.get(a) {
                            nodes[n].uses.push(Use::Arg { call: c, j });
                        }
                    }
                    continue;
                }
                _ => {}
            }
            for o in i.operands() {
                if let Some(&n) = member.get(&o) {
                    nodes[n].other = true;
                }
            }
        }
    }
    let mut rets = Vec::new();
    for (bi, b) in f.blocks.iter().enumerate() {
        let Some(t) = &b.term else { continue };
        if let Terminator::Return { lo: Some(v), hi: None, more } = t
            && more.is_empty()
        {
            let site = if let Some(&n) = member.get(v) {
                nodes[n].uses.push(Use::Ret);
                RetSite::Node(n)
            } else if f.ret_ty == Some(MirTy::NicheOpt) && f.const_i64(through_casts(f, *v)) == Some(0) {
                RetSite::None
            } else {
                RetSite::Other
            };
            rets.push((bi, site));
            continue;
        }
        if matches!(t, Terminator::Return { .. }) {
            rets.push((bi, RetSite::Other));
        }
        for u in t.uses() {
            if let Some(&n) = member.get(&u) {
                nodes[n].other = true;
            }
        }
    }
    let wide_ret = twin && f.ret_ty.is_some_and(object_word) && f.ret_hi_ty.is_none() && f.ret_more_tys.is_empty();
    let wanted = wanted(&nodes);
    Body { nodes, member, calls, rets, params, twinnable: twin, wide_ret, wanted }
}

/// [`Body::wanted`]: each node's group (joined by φs and nesting) meets a
/// call edge or a φ.
fn wanted(nodes: &[Node]) -> Vec<bool> {
    let mut group: Vec<usize> = (0..nodes.len()).collect();
    fn find(g: &mut [usize], mut x: usize) -> usize {
        while g[x] != x {
            g[x] = g[g[x]];
            x = g[x];
        }
        x
    }
    let join = |a: usize, b: usize, group: &mut Vec<usize>| {
        let (a, b) = (find(group, a), find(group, b));
        group[a] = b;
    };
    for (n, node) in nodes.iter().enumerate() {
        if let Src::Item { parent, .. } | Src::ErrOf { parent } = node.src {
            join(n, parent, &mut group);
        }
        for u in &node.uses {
            match *u {
                Use::Phi(p) | Use::Within { parent: p, .. } | Use::ErrIn { parent: p } => join(n, p, &mut group),
                _ => {}
            }
        }
    }
    let mut hit = vec![false; nodes.len()];
    for (n, node) in nodes.iter().enumerate() {
        if matches!(node.src, Src::Param(_) | Src::Call(_) | Src::Phi(_)) || node.uses.iter().any(|u| matches!(u, Use::Arg { .. } | Use::Ret)) {
            let g = find(&mut group, n);
            hit[g] = true;
        }
    }
    (0..nodes.len()).map(|n| hit[find(&mut group, n)]).collect()
}

/// A slot whose words several nodes share: a node, a body's parameter, a
/// body's result.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Key {
    Node(usize, usize),
    Param(usize, usize),
    Ret(usize),
}

/// A class's tuple: per item, the class of the item and its word type as a
/// leaf, and the tuple's kind and per-item word kinds.
#[derive(Clone, Debug)]
struct Tuple {
    slots: Vec<(usize, MirTy, u8)>,
    kind: MirAllocKind,
}

/// A class of [`Key`]s (and of tuple items) that must agree on their words.
#[derive(Clone, Debug, Default)]
struct Class {
    tuple: Option<Tuple>,
    niche: Option<bool>,
    /// A `Result`'s class: its tuple is the `Ok` payload's.
    res: Option<bool>,
    /// A `Result`'s `Err` payload: its class and its word type as a leaf.
    err: Option<(usize, MirTy)>,
    bad: bool,
    /// Some member stays an object (an item that is no node, a read the
    /// pass does not take): the class is never nested in another's words.
    fixed: bool,
}

impl Class {
    fn set_niche(&mut self, niche: bool) {
        match self.niche {
            Some(n) if n != niche => self.bad = true,
            _ => self.niche = Some(niche),
        }
    }

    fn set_res(&mut self, res: bool) {
        match self.res {
            Some(r) if r != res => self.bad = true,
            _ => self.res = Some(res),
        }
    }
}

struct Classes {
    parent: Vec<usize>,
    data: Vec<Class>,
    index: HashMap<Key, usize>,
}

impl Classes {
    fn fresh(&mut self, c: Class) -> usize {
        let n = self.parent.len();
        self.parent.push(n);
        self.data.push(c);
        n
    }

    fn id(&mut self, k: Key) -> usize {
        if let Some(&i) = self.index.get(&k) {
            return i;
        }
        let n = self.fresh(Class::default());
        self.index.insert(k, n);
        n
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    /// Join two classes, and their tuples' items pairwise, at any depth.
    fn unify(&mut self, a: usize, b: usize) {
        let mut work = vec![(a, b)];
        while let Some((a, b)) = work.pop() {
            let (a, b) = (self.find(a), self.find(b));
            if a == b {
                continue;
            }
            self.parent[a] = b;
            let ca = std::mem::take(&mut self.data[a]);
            let cb = &mut self.data[b];
            cb.bad |= ca.bad;
            cb.fixed |= ca.fixed;
            if let Some(n) = ca.niche {
                cb.set_niche(n);
            }
            if let Some(r) = ca.res {
                cb.set_res(r);
            }
            match (ca.err, &mut cb.err) {
                (None, _) => {}
                (Some(e), slot @ None) => *slot = Some(e),
                (Some((c, ty)), Some((uc, uty))) => {
                    match union_word(ty, *uty) {
                        Some(j) => *uty = j,
                        None => cb.bad = true,
                    }
                    work.push((c, *uc));
                }
            }
            match (ca.tuple, &mut cb.tuple) {
                (None, _) => {}
                (Some(t), slot @ None) => *slot = Some(t),
                (Some(t), Some(u)) => {
                    if t.slots.len() != u.slots.len() || t.kind != u.kind {
                        cb.bad = true;
                        continue;
                    }
                    for (k, &(s, ty, kind)) in t.slots.iter().enumerate() {
                        let (us, uty, ukind) = u.slots[k];
                        match join_word(ty, uty) {
                            Some(j) => u.slots[k] = (us, j, if kind == ukind { kind } else { common::WORD_UNKNOWN }),
                            None => cb.bad = true,
                        }
                        work.push((s, us));
                    }
                }
            }
        }
    }

    fn union(&mut self, a: Key, b: Key) {
        let (a, b) = (self.id(a), self.id(b));
        self.unify(a, b);
    }

    fn of(&mut self, k: Key) -> usize {
        let i = self.id(k);
        self.find(i)
    }

    /// The class of item `slot` of class `c`'s tuple.
    fn slot(&mut self, c: usize, slot: usize) -> Option<usize> {
        let c = self.find(c);
        let s = self.data[c].tuple.as_ref()?.slots.get(slot)?.0;
        Some(self.find(s))
    }
}

/// A class's words, flattened.
#[derive(Clone, Debug)]
struct Lay {
    words: Vec<MirTy>,
    /// Per item of the tuple, where its words are.
    slots: Vec<Slot>,
    /// An `Option` of the tuple: its last word is `Some`'s flag.
    niche: bool,
    /// A `Result` of the tuple (its `Ok`): its last word is `Ok`'s flag,
    /// and its `Err` payload shares the word at `err` with an `Ok` leaf.
    res: Option<usize>,
    kind: MirAllocKind,
    kinds: common::PackedKinds,
    /// It and every tuple nested in it can be built again from the words
    /// with no branch (no `Option` in it).
    rebuild: bool,
}

#[derive(Clone, Debug)]
enum Slot {
    /// One word, at this index.
    Leaf(usize),
    /// A tuple nested in place: its words start at `at`.
    Nested { at: usize, lay: Rc<Lay> },
}

/// Class `c`'s words: its tuple's items' leaves, a nested tuple's in place,
/// within the budget (one rule at any depth). A tuple whose nested words do
/// not fit keeps its items as one word each.
fn layout(cls: &mut Classes, c: usize, memo: &mut HashMap<usize, Option<Rc<Lay>>>, busy: &mut HashSet<usize>) -> Option<Rc<Lay>> {
    let c = cls.find(c);
    if let Some(l) = memo.get(&c) {
        return l.clone();
    }
    // A class nested in itself has no finite words.
    if !busy.insert(c) {
        return None;
    }
    let class = cls.data[c].clone();
    let out = (|| {
        if class.bad {
            return None;
        }
        let t = class.tuple?;
        let niche = class.niche == Some(true);
        let res = class.res == Some(true);
        if niche && res {
            return None;
        }
        // An `Err` that is never made, or a `Result` with no `Err` payload
        // type, has no word to share.
        let err = if res { Some(class.err?.1) } else { None };
        let inner: Vec<Option<Rc<Lay>>> = t
            .slots
            .iter()
            .map(|&(s, _, _)| {
                let s = cls.find(s);
                if cls.data[s].fixed || cls.data[s].tuple.is_none() { None } else { layout(cls, s, memo, busy) }
            })
            .collect();
        let kinds = common::pack_word_kinds(t.slots.iter().map(|&(_, _, k)| k));
        for nest in [true, false] {
            let mut words = Vec::new();
            let mut slots = Vec::new();
            let mut rebuild = !niche && !res;
            for (k, &(_, ty, _)) in t.slots.iter().enumerate() {
                match inner[k].as_ref().filter(|_| nest) {
                    Some(l) => {
                        slots.push(Slot::Nested { at: words.len(), lay: l.clone() });
                        words.extend(l.words.iter().copied());
                        rebuild &= l.rebuild;
                    }
                    None => {
                        slots.push(Slot::Leaf(words.len()));
                        words.push(ty);
                    }
                }
            }
            if niche {
                words.push(MirTy::Bool);
            }
            // The `Err` payload rides a leaf of the `Ok`'s: one of its own
            // type if there is one, else the first it can share.
            let mut res_at = None;
            if let Some(e) = err {
                let leaves = slots.iter().filter_map(|s| if let Slot::Leaf(at) = s { Some(*at) } else { None });
                let at = leaves.clone().find(|&at| words[at] == e).or_else(|| leaves.clone().find(|&at| union_word(words[at], e).is_some()));
                match at {
                    Some(at) => {
                        words[at] = union_word(words[at], e).expect("a shared word");
                        res_at = Some(at);
                        words.push(MirTy::Bool);
                    }
                    None => continue,
                }
            }
            if (2..=MAX_WORDS).contains(&words.len()) {
                return Some(Rc::new(Lay { words, slots, niche, res: res_at, kind: t.kind, kinds, rebuild }));
            }
            if !inner.iter().any(Option::is_some) {
                break;
            }
        }
        None
    })();
    busy.remove(&c);
    memo.insert(c, out.clone());
    out
}

/// Build the object of `lay` from its words `ws` (nested tuples first):
/// its value, `dest` when given.
fn build(f: &mut MirFunc, lay: &Lay, ws: &[ValueId], dest: Option<ValueId>, out: &mut Vec<MirInst>) -> ValueId {
    let mut elems = Vec::with_capacity(lay.slots.len());
    for s in &lay.slots {
        match s {
            Slot::Leaf(at) => elems.push(ws[*at]),
            Slot::Nested { at, lay: l } => elems.push(build(f, l, &ws[*at..*at + l.words.len()], None, out)),
        }
    }
    let d = dest.unwrap_or_else(|| new_value(f, MirTy::HeapRef));
    out.push(MirInst::Alloc { dest: d, kind: lay.kind, elems, kinds: lay.kinds });
    d
}

/// The two ways a body runs: as itself (its parameters and result are
/// objects) and as its twin.
const ORIG: usize = 0;
const TWIN: usize = 1;

struct Program<'a> {
    bodies: &'a [NativeBody],
    info: Vec<Body>,
    /// Each node's class's words, by body and node (`None`: never words).
    lay: Vec<Vec<Option<Rc<Lay>>>>,
    param_lay: Vec<Vec<Option<Rc<Lay>>>>,
    ret_lay: Vec<Option<Rc<Lay>>>,
    // The fixpoint's answers.
    param_w: Vec<Vec<bool>>,
    ret_w: Vec<bool>,
    viable: [Vec<Vec<bool>>; 2],
    switch: [Vec<Vec<bool>>; 2],
}

impl<'a> Program<'a> {
    fn new(bodies: &'a [NativeBody]) -> Self {
        let by_label: HashMap<Label, usize> = bodies.iter().enumerate().filter_map(|(i, b)| Some((b.entry?, i))).collect();
        let info: Vec<Body> = bodies.iter().map(|b| read_body(&b.name, &b.mir, &by_label)).collect();
        // Shapes: unify every edge words could cross, and each fresh
        // tuple's items with what they are made of.
        let mut cls = Classes { parent: Vec::new(), data: Vec::new(), index: HashMap::new() };
        for (bi, b) in info.iter().enumerate() {
            let f = &bodies[bi].mir;
            for (n, node) in b.nodes.iter().enumerate() {
                let k = Key::Node(bi, n);
                let id = cls.id(k);
                if node.niche {
                    let c = cls.find(id);
                    cls.data[c].set_niche(true);
                } else if !matches!(node.src, Src::None) {
                    let c = cls.find(id);
                    cls.data[c].set_niche(false);
                }
                if node.res {
                    let c = cls.find(id);
                    cls.data[c].set_res(true);
                } else if !matches!(node.src, Src::None) {
                    let c = cls.find(id);
                    cls.data[c].set_res(false);
                }
                match &node.src {
                    Src::Param(j) => cls.union(k, Key::Param(bi, *j)),
                    Src::Call(c) => {
                        if let Some(t) = b.calls[*c].callee {
                            cls.union(k, Key::Ret(t));
                        }
                    }
                    Src::Alloc { items, kind, kinds } => {
                        let slots = items
                            .iter()
                            .enumerate()
                            .map(|(s, e)| {
                                let word = common::packed_word_kind(*kinds, s);
                                let c = match b.member.get(e) {
                                    Some(&m) => cls.id(Key::Node(bi, m)),
                                    None => cls.fresh(Class { fixed: true, ..Class::default() }),
                                };
                                (c, item_ty(f.ty(*e), word), word)
                            })
                            .collect();
                        let t = cls.fresh(Class { tuple: Some(Tuple { slots, kind: *kind }), ..Class::default() });
                        cls.unify(id, t);
                    }
                    Src::Opaque => {
                        let c = cls.find(id);
                        cls.data[c].bad = true;
                    }
                    // The payload is one word, whatever it holds.
                    Src::Err { payload } => {
                        let e = cls.fresh(Class { fixed: true, ..Class::default() });
                        let r = cls.fresh(Class { res: Some(true), err: Some((e, f.ty(*payload))), ..Class::default() });
                        cls.unify(id, r);
                    }
                    _ => {}
                }
                for u in &node.uses {
                    match *u {
                        Use::Arg { call, j } => {
                            if let Some(t) = b.calls[call].callee {
                                cls.union(k, Key::Param(t, j));
                            }
                        }
                        Use::Ret => cls.union(k, Key::Ret(bi)),
                        Use::Phi(p) => cls.union(k, Key::Node(bi, p)),
                        _ => {}
                    }
                }
            }
            if b.wide_ret {
                let c = cls.of(Key::Ret(bi));
                cls.data[c].set_niche(f.ret_ty == Some(MirTy::NicheOpt));
                cls.data[c].set_res(f.ret_ty == Some(MirTy::NicheRes));
            }
        }
        // An item load's node is its parent's item, once the parent's
        // tuple is known (an item of an item waits for its parent's).
        let mut pending: Vec<(usize, usize, usize, usize)> = Vec::new();
        for (bi, b) in info.iter().enumerate() {
            for (n, node) in b.nodes.iter().enumerate() {
                if let Src::Item { parent, slot } = node.src {
                    pending.push((bi, n, parent, slot));
                }
            }
        }
        loop {
            let before = pending.len();
            pending.retain(|&(bi, n, parent, slot)| {
                let p = cls.of(Key::Node(bi, parent));
                if cls.data[p].tuple.is_none() {
                    return true;
                }
                match cls.slot(p, slot) {
                    Some(s) => {
                        let k = cls.id(Key::Node(bi, n));
                        cls.unify(k, s);
                    }
                    None => {
                        let c = cls.of(Key::Node(bi, n));
                        cls.data[c].bad = true;
                    }
                }
                false
            });
            if pending.len() == before {
                break;
            }
        }
        // What stays an object is never nested in another's words.
        for (bi, b) in info.iter().enumerate() {
            for (n, node) in b.nodes.iter().enumerate() {
                let c = cls.of(Key::Node(bi, n));
                if node.other || matches!(node.src, Src::Opaque) {
                    cls.data[c].fixed = true;
                }
                for u in &node.uses {
                    if let Use::Load { dest, slot } = *u
                        && !b.member.contains_key(&dest)
                        && let Some(s) = cls.slot(c, slot)
                    {
                        cls.data[s].fixed = true;
                    }
                }
            }
        }
        let mut memo = HashMap::new();
        let mut busy = HashSet::new();
        let mut lay_of = |k: Key, cls: &mut Classes| -> Option<Rc<Lay>> {
            let c = cls.of(k);
            layout(cls, c, &mut memo, &mut busy)
        };
        let mut lay = Vec::new();
        let mut param_lay = Vec::new();
        let mut ret_lay = Vec::new();
        for (bi, b) in info.iter().enumerate() {
            lay.push((0..b.nodes.len()).map(|n| lay_of(Key::Node(bi, n), &mut cls)).collect::<Vec<_>>());
            param_lay.push(
                (0..b.params.len())
                    .map(|j| match b.params[j] {
                        Some(_) if b.twinnable => lay_of(Key::Param(bi, j), &mut cls),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            );
            ret_lay.push(if b.wide_ret { lay_of(Key::Ret(bi), &mut cls) } else { None });
        }
        let param_w = param_lay.iter().map(|p| p.iter().map(Option::is_some).collect()).collect();
        let ret_w = ret_lay.iter().map(Option::is_some).collect();
        let viable = [0, 1].map(|_| lay.iter().map(|w| w.iter().map(Option::is_some).collect()).collect());
        let switch = [0, 1].map(|_| info.iter().map(|b| vec![true; b.calls.len()]).collect());
        Program { bodies, info, lay, param_lay, ret_lay, param_w, ret_w, viable, switch }
    }

    fn has_twin(&self, t: usize) -> bool {
        self.ret_w[t] || self.param_w[t].iter().any(|&w| w)
    }

    /// Item `slot` of node `n` of body `b` is a tuple nested in its words.
    fn nested(&self, b: usize, n: usize, slot: usize) -> bool {
        self.lay[b][n].as_ref().is_some_and(|l| matches!(l.slots.get(slot), Some(Slot::Nested { .. })))
    }

    /// Whether node `n` of body `b` keeps no object when `b` runs as `ctx`.
    fn node_ok(&self, b: usize, ctx: usize, n: usize) -> bool {
        let node = &self.info[b].nodes[n];
        let Some(lay) = self.lay[b][n].as_ref() else { return false };
        if node.other || !self.info[b].wanted[n] {
            return false;
        }
        let body = &self.info[b];
        let f = &self.bodies[b].mir;
        let viable = |m: usize| self.viable[ctx][b][m];
        let src = match node.src {
            Src::Alloc { ref items, .. } => lay.slots.iter().zip(items).all(|(s, e)| match s {
                Slot::Leaf(at) => fits(lay.words[*at], f.ty(*e)),
                Slot::Nested { .. } => body.member.get(e).is_some_and(|&m| viable(m)),
            }),
            Src::Param(j) => ctx == TWIN && self.param_w[b][j],
            Src::Call(c) => self.switch[ctx][b][c] && body.calls[c].callee.is_some_and(|t| self.ret_w[t]),
            Src::Phi(ref args) => args.iter().all(|(_, a)| body.member.get(a).is_some_and(|&m| viable(m))),
            Src::Item { parent, slot } => viable(parent) && self.nested(b, parent, slot),
            Src::None => lay.niche,
            Src::Err { payload } => lay.res.is_some_and(|at| union_word(lay.words[at], f.ty(payload)) == Some(lay.words[at])),
            Src::ErrOf { .. } | Src::Opaque => false,
        };
        src && node.uses.iter().all(|u| match *u {
            Use::Load { dest, slot } => match lay.slots.get(slot) {
                Some(Slot::Leaf(at)) => fits(lay.words[*at], f.ty(dest)),
                Some(Slot::Nested { .. }) => body.member.get(&dest).is_some_and(|&m| viable(m)),
                None => false,
            },
            Use::Test => lay.niche,
            Use::ErrBit { .. } => lay.res.is_some(),
            Use::ErrPayload { dest } => lay.res.is_some_and(|at| MirCastKind::Word.allows(lay.words[at], f.ty(dest)) || lay.words[at].word_class() == f.ty(dest).word_class()),
            // An `Err`'s payload is a word of its own, not taken apart.
            Use::ErrIn { .. } => false,
            Use::Arg { call, j } => self.switch[ctx][b][call] && body.calls[call].callee.is_some_and(|t| self.param_w[t].get(j) == Some(&true)),
            Use::Ret => ctx == TWIN && self.ret_w[b],
            Use::Phi(p) => viable(p),
            Use::Within { parent, slot } => viable(parent) && self.nested(b, parent, slot),
            Use::Drop { .. } => true,
        })
    }

    /// Whether call `c` of body `b` running as `ctx` can enter its callee's
    /// twin, and gains by it.
    fn call_ok(&self, b: usize, ctx: usize, c: usize) -> bool {
        let call = &self.info[b].calls[c];
        let Some(t) = call.callee else { return false };
        if !self.has_twin(t) || !call.one_word && self.ret_w[t] {
            return false;
        }
        let body = &self.info[b];
        let mut gain = false;
        for (j, &w) in self.param_w[t].iter().enumerate() {
            if !w {
                continue;
            }
            let Some(&n) = call.args.get(j).and_then(|a| body.member.get(a)) else { return false };
            if !self.viable[ctx][b][n] {
                return false;
            }
            gain = true;
        }
        if call.args.len() != self.param_w[t].len() {
            return false;
        }
        if self.ret_w[t] {
            let dest = body.member.get(&call.dest).copied();
            let kept = dest.is_none_or(|n| !self.viable[ctx][b][n]);
            if kept {
                // A caller that keeps the object builds it again from the
                // words, past the call; an `Option` would need a branch.
                if !self.ret_lay[t].as_ref().is_some_and(|l| l.rebuild) {
                    return false;
                }
            } else {
                gain = true;
            }
        }
        gain
    }

    fn ret_ok(&self, b: usize) -> bool {
        let info = &self.info[b];
        let niche = self.ret_lay[b].as_ref().is_some_and(|l| l.niche);
        !info.rets.is_empty()
            && info.rets.iter().any(|(_, r)| matches!(r, RetSite::Node(_)))
            && info.rets.iter().all(|&(_, r)| match r {
                RetSite::Node(n) => self.viable[TWIN][b][n],
                RetSite::None => niche,
                RetSite::Other => false,
            })
    }

    fn solve(&mut self) {
        loop {
            let mut changed = false;
            for b in 0..self.info.len() {
                for ctx in [ORIG, TWIN] {
                    if ctx == TWIN && !self.info[b].twinnable {
                        continue;
                    }
                    for n in 0..self.info[b].nodes.len() {
                        if self.viable[ctx][b][n] && !self.node_ok(b, ctx, n) {
                            self.viable[ctx][b][n] = false;
                            changed = true;
                        }
                    }
                    for c in 0..self.info[b].calls.len() {
                        if self.switch[ctx][b][c] && !self.call_ok(b, ctx, c) {
                            self.switch[ctx][b][c] = false;
                            changed = true;
                        }
                    }
                }
                for j in 0..self.param_w[b].len() {
                    if self.param_w[b][j] && !self.info[b].params[j].is_some_and(|n| self.viable[TWIN][b][n]) {
                        self.param_w[b][j] = false;
                        changed = true;
                    }
                }
                if self.ret_w[b] && !self.ret_ok(b) {
                    self.ret_w[b] = false;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// The twins some running body calls: the bodies' own switched calls,
    /// then those of the twins they reach.
    fn twins(&self) -> Vec<usize> {
        let mut need: Vec<usize> = Vec::new();
        let mut seen: HashSet<usize> = HashSet::new();
        let mut work: Vec<(usize, usize)> = (0..self.info.len()).map(|b| (b, ORIG)).collect();
        while let Some((b, ctx)) = work.pop() {
            for (c, call) in self.info[b].calls.iter().enumerate() {
                if self.switch[ctx][b][c]
                    && let Some(t) = call.callee
                    && seen.insert(t)
                {
                    need.push(t);
                    work.push((t, TWIN));
                }
            }
        }
        need.sort_unstable();
        need
    }

    /// Whether body `b` running as `ctx` takes any node apart.
    fn changes(&self, b: usize, ctx: usize) -> bool {
        self.switch[ctx][b].iter().any(|&s| s) || self.viable[ctx][b].iter().any(|&v| v)
    }

    /// Body `b` running as `ctx`, rewritten: its dissolved nodes' objects
    /// gone, its switched calls entering twins (`twin_of`).
    fn rewrite(&self, b: usize, ctx: usize, twin_of: &HashMap<usize, Label>) -> (MirFunc, usize) {
        let mut f = self.bodies[b].mir.clone();
        let info = &self.info[b];
        let viable = &self.viable[ctx][b];
        // Constants a rewrite adds: in place of a dissolved tuple, and at
        // the end of a block.
        let mut at: HashMap<ValueId, Vec<MirInst>> = HashMap::new();
        let mut tail: HashMap<usize, Vec<MirInst>> = HashMap::new();
        // Each dissolved node's words.
        let mut node_words: HashMap<usize, Vec<ValueId>> = HashMap::new();
        let mut gone: HashSet<ValueId> = HashSet::new();
        let mut subst: HashMap<ValueId, ValueId> = HashMap::new();
        let lay = |n: usize| self.lay[b][n].clone().expect("a dissolved node's words");
        if ctx == TWIN {
            let mut params = Vec::new();
            for (j, &p) in f.params.clone().iter().enumerate() {
                if self.param_w[b][j] {
                    let tys = self.param_lay[b][j].clone().expect("a parameter's words").words.clone();
                    let ws: Vec<ValueId> = tys.iter().map(|&t| new_value(&mut f, t)).collect();
                    params.extend(ws.iter().copied());
                    node_words.insert(info.params[j].expect("a parameter's node"), ws);
                } else {
                    params.push(p);
                }
            }
            f.params = params;
        }
        // Switched calls: their result words.
        let mut call_words: HashMap<usize, Vec<ValueId>> = HashMap::new();
        for (c, call) in info.calls.iter().enumerate() {
            if !self.switch[ctx][b][c] {
                continue;
            }
            let t = call.callee.expect("a switched call's callee");
            if self.ret_w[t] {
                let tys = self.ret_lay[t].clone().expect("a result's words").words.clone();
                let dissolved = info.member.get(&call.dest).copied().filter(|&n| viable[n]);
                // A dissolved result's first word keeps the result's name;
                // a kept one names the object rebuilt after the call.
                let mut ws: Vec<ValueId> = Vec::with_capacity(tys.len());
                for (k, &ty) in tys.iter().enumerate() {
                    if k == 0 && dissolved.is_some() {
                        f.types[call.dest.index()] = ty;
                        ws.push(call.dest);
                    } else {
                        ws.push(new_value(&mut f, ty));
                    }
                }
                if let Some(n) = dissolved {
                    node_words.insert(n, ws.clone());
                }
                call_words.insert(c, ws);
            }
        }
        // A dissolved φ's words are φs of its incoming nodes' words; a
        // `None`'s are zeros and a `false` flag.
        for (n, node) in info.nodes.iter().enumerate() {
            if !viable[n] {
                continue;
            }
            match node.src {
                Src::Phi(_) => {
                    let ws = lay(n).words.iter().map(|&t| new_value(&mut f, t)).collect();
                    node_words.insert(n, ws);
                }
                Src::None => {
                    let mut ws = Vec::new();
                    for &t in &lay(n).words {
                        let z = new_value(&mut f, t);
                        at.entry(node.members[0]).or_default().push(MirInst::Const { dest: z, c: zero(t) });
                        ws.push(z);
                    }
                    node_words.insert(n, ws);
                }
                // An `Err`: its payload in the word it shares, zeros in the
                // `Ok`'s others, a `false` flag.
                Src::Err { payload } => {
                    let l = lay(n);
                    let share = l.res.expect("a Result's shared word");
                    let mut out = Vec::new();
                    let mut ws = Vec::new();
                    for (k, &t) in l.words.iter().enumerate() {
                        if k == share {
                            ws.push(as_word(&mut f, payload, t, &mut out));
                        } else {
                            let z = new_value(&mut f, t);
                            out.push(MirInst::Const { dest: z, c: zero(t) });
                            ws.push(z);
                        }
                    }
                    at.entry(node.members[0]).or_default().extend(out);
                    node_words.insert(n, ws);
                }
                _ => {}
            }
        }
        // Fresh tuples (their nested items' words first) and item loads
        // (their parents' words first), to a fixed point.
        loop {
            let mut grew = false;
            for (n, node) in info.nodes.iter().enumerate() {
                if !viable[n] || node_words.contains_key(&n) {
                    continue;
                }
                let l = lay(n);
                let ws = match &node.src {
                    Src::Alloc { items, .. } => {
                        let mut ws = Vec::with_capacity(l.words.len());
                        let mut ready = true;
                        let mut casts = Vec::new();
                        for (s, e) in l.slots.iter().zip(items) {
                            match s {
                                Slot::Leaf(k) => ws.push(as_word(&mut f, *e, l.words[*k], &mut casts)),
                                Slot::Nested { .. } => match node_words.get(&info.member[e]) {
                                    Some(inner) => ws.extend(inner.iter().copied()),
                                    None => ready = false,
                                },
                            }
                        }
                        if !ready {
                            continue;
                        }
                        at.entry(node.members[0]).or_default().extend(casts);
                        if l.niche || l.res.is_some() {
                            // `Some`'s (`Ok`'s) flag: where the tuple was
                            // made, or, read by nothing but a return, at the
                            // end of its block.
                            let t = new_value(&mut f, MirTy::Bool);
                            let flag = MirInst::Const { dest: t, c: MirConst::Bool(true) };
                            let ret = info.rets.iter().find(|(_, r)| matches!(r, RetSite::Node(m) if *m == n)).map(|(bi, _)| *bi);
                            match ret.filter(|_| !node.uses.iter().any(|u| matches!(u, Use::Test | Use::ErrBit { .. }))) {
                                Some(bi) => tail.entry(bi).or_default().push(flag),
                                None => at.entry(node.members[0]).or_default().push(flag),
                            }
                            ws.push(t);
                        }
                        ws
                    }
                    Src::Item { parent, slot } => {
                        let Some(pw) = node_words.get(parent) else { continue };
                        let Some(Slot::Nested { at: off, lay: inner }) = lay(*parent).slots.get(*slot).cloned() else {
                            unreachable!("a dissolved item is nested in its parent's words")
                        };
                        pw[off..off + inner.words.len()].to_vec()
                    }
                    _ => unreachable!("words of a dissolved node"),
                };
                node_words.insert(n, ws);
                grew = true;
            }
            if !grew {
                break;
            }
        }
        for (n, node) in info.nodes.iter().enumerate() {
            if !viable[n] {
                continue;
            }
            gone.extend(node.members.iter().copied());
            let l = lay(n);
            for u in &node.uses {
                match *u {
                    Use::Load { dest, slot } => {
                        if let Slot::Leaf(i) = l.slots[slot] {
                            let w = node_words[&n][i];
                            let to = f.ty(dest);
                            if f.ty(w).word_class() == to.word_class() {
                                subst.insert(dest, w);
                            } else {
                                at.entry(dest).or_default().push(MirInst::Cast { dest, kind: MirCastKind::Word, to, src: w });
                            }
                        }
                        gone.insert(dest);
                    }
                    // Whether it is an `Err`: its flag's `not`, as the `int`
                    // the test reads.
                    Use::ErrBit { dest } => {
                        let flag = *node_words[&n].last().expect("a Result's flag");
                        let not = new_value(&mut f, MirTy::Bool);
                        let to = f.ty(dest);
                        let c = at.entry(dest).or_default();
                        c.push(MirInst::Unary { op: MirUnaryOp::Not, dest: not, src: flag });
                        c.push(MirInst::Cast { dest, kind: MirCastKind::Word, to, src: not });
                        gone.insert(dest);
                    }
                    Use::ErrPayload { dest } => {
                        let w = node_words[&n][l.res.expect("a Result's shared word")];
                        let to = f.ty(dest);
                        if f.ty(w) == to {
                            subst.insert(dest, w);
                        } else {
                            at.entry(dest).or_default().push(MirInst::Cast { dest, kind: MirCastKind::Word, to, src: w });
                        }
                        gone.insert(dest);
                    }
                    Use::Drop { dest, keep } => {
                        gone.insert(dest);
                        if keep {
                            for (k, &w) in node_words[&n].iter().enumerate() {
                                if l.words[k].is_heap_word() || l.words[k] == MirTy::MaybeObj {
                                    let t = new_value(&mut f, MirTy::Bool);
                                    at.entry(dest).or_default().push(MirInst::KeepAlive { dest: t, value: w });
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        for (n, node) in info.nodes.iter().enumerate() {
            if viable[n]
                && let Src::Phi(args) = &node.src
            {
                let tys = lay(n).words.clone();
                let phis = (0..tys.len())
                    .map(|k| MirInst::Phi { dest: node_words[&n][k], ty: tys[k], args: args.iter().map(|&(blk, a)| (blk, node_words[&info.member[&a]][k])).collect() })
                    .collect::<Vec<_>>();
                at.entry(node.members[0]).or_default().extend(phis);
            }
        }
        let flag_of = |v: ValueId| -> Option<ValueId> {
            let n = *info.member.get(&v)?;
            viable[n].then(|| *node_words[&n].last().expect("words"))
        };
        let mut moved = 0;
        let call_index: HashMap<ValueId, usize> = info.calls.iter().enumerate().map(|(c, call)| (call.dest, c)).collect();
        for bi in 0..f.blocks.len() {
            let insts = std::mem::take(&mut f.blocks[bi].insts);
            let mut out = Vec::with_capacity(insts.len());
            for mut inst in insts {
                if let MirInst::Unary { op: MirUnaryOp::Not, src, .. } = &mut inst
                    && let Some(flag) = flag_of(*src)
                {
                    *src = flag;
                }
                let mut rebuild = Vec::new();
                if let MirInst::Call { dest, dest_hi, dest_more, target, args } = &mut inst
                    && let Some(&c) = call_index.get(dest)
                    && self.switch[ctx][b][c]
                {
                    let t = info.calls[c].callee.expect("callee");
                    let mut out_args = Vec::new();
                    for (j, a) in args.iter().enumerate() {
                        if self.param_w[t][j] {
                            out_args.extend(node_words[&info.member[a]].iter().copied());
                        } else {
                            out_args.push(*a);
                        }
                    }
                    *args = out_args;
                    let label = twin_of[&t];
                    if f.tail_targets.contains(target) {
                        f.tail_targets.insert(label);
                    }
                    *target = label;
                    if let Some(ws) = call_words.get(&c) {
                        let old = *dest;
                        *dest = ws[0];
                        *dest_hi = Some(ws[1]);
                        *dest_more = ws[2..].to_vec();
                        if old != ws[0]
                            && let Some(loc) = f.value_locs.get(&old).copied()
                        {
                            f.value_locs.insert(ws[0], loc);
                        }
                        if old != ws[0] && f.steady_calls.remove(&old) {
                            f.steady_calls.insert(ws[0]);
                        }
                        if !info.member.get(&old).is_some_and(|&n| viable[n]) {
                            // The caller keeps the object: build it here.
                            let l = self.ret_lay[t].clone().expect("a result's words");
                            build(&mut f, &l, ws, Some(old), &mut rebuild);
                        }
                    }
                    moved += 1;
                }
                let d = inst.dest();
                if !gone.contains(&d) || matches!(inst, MirInst::Call { .. }) {
                    out.push(inst);
                } else if let Some(c) = at.remove(&d) {
                    out.extend(c);
                }
                out.extend(rebuild);
            }
            f.blocks[bi].insts = out;
        }
        // Returns of words.
        if ctx == TWIN && self.ret_w[b] {
            let tys = self.ret_lay[b].clone().expect("a result's words").words.clone();
            for &(bi, site) in &info.rets {
                let ws: Vec<ValueId> = match site {
                    RetSite::Node(n) => node_words[&n].clone(),
                    RetSite::None => tys
                        .iter()
                        .map(|&ty| {
                            let z = new_value(&mut f, ty);
                            tail.entry(bi).or_default().push(MirInst::Const { dest: z, c: zero(ty) });
                            z
                        })
                        .collect(),
                    RetSite::Other => unreachable!("a twin returns words from every return"),
                };
                f.blocks[bi].term = Some(Terminator::Return { lo: Some(ws[0]), hi: Some(ws[1]), more: ws[2..].to_vec() });
            }
            f.ret_ty = Some(tys[0]);
            f.ret_hi_ty = Some(tys[1]);
            f.ret_more_tys = tys[2..].to_vec();
            f.ret_layout = crate::layout::MirLayout::TwoSlot;
        }
        for (bi, consts) in tail {
            f.blocks[bi].insts.extend(consts);
        }
        f.replace_uses(&subst);
        f.bound_words.retain(|v| !gone.contains(v));
        f.local_binds.retain(|(_, v)| !gone.contains(v));
        f.debug_slots.retain(|_, v| !gone.contains(v));
        dce(&mut f);
        (f, moved)
    }
}

/// Give each body a `$words` twin when a direct call can use it, and point
/// those calls at it; take apart the tuples a body's φs carry. Returns how
/// many calls moved.
pub fn wide_call_twins(bodies: &mut Vec<NativeBody>) -> usize {
    let mut prog = Program::new(bodies);
    if !(0..prog.info.len()).any(|b| prog.has_twin(b) || prog.viable[ORIG][b].iter().any(|&v| v)) {
        return 0;
    }
    prog.solve();
    let need = prog.twins();
    let first = bodies.iter().flat_map(|b| b.entry.into_iter().chain(b.mir.code_targets())).map(|l| l.0).max().unwrap_or(0) + 1;
    let twin_of: HashMap<usize, Label> = need.iter().zip(first..).map(|(&t, l)| (t, Label(l))).collect();
    let mut moved = 0;
    let mut rewritten: Vec<(usize, MirFunc)> = Vec::new();
    for b in 0..bodies.len() {
        if prog.changes(b, ORIG) {
            let (f, n) = prog.rewrite(b, ORIG, &twin_of);
            moved += n;
            rewritten.push((b, f));
        }
    }
    let mut twins = Vec::new();
    for &t in &need {
        let (mut f, n) = prog.rewrite(t, TWIN, &twin_of);
        moved += n;
        let name = format!("{}{}", bodies[t].name, common::WORDS_TWIN_SUFFIX);
        f.name = name.clone();
        // Only direct calls reach a twin: no function value's entry.
        f.pair_twin = None;
        twins.push(NativeBody { name, entry: Some(twin_of[&t]), mir: f });
    }
    drop(prog);
    for (b, f) in rewritten {
        bodies[b].mir = f;
    }
    bodies.extend(twins);
    moved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::CallAbi;
    use crate::mir::builder::MirBuilder;

    fn scalar3() -> common::PackedKinds {
        (0..3).fold(0, |k, i| k | (common::PackedKinds::from(common::WORD_SCALAR) << (2 * i)))
    }

    /// `sum3(t) = t.0 + t.1 + t.2` at label 1.
    fn sum3() -> NativeBody {
        let mut b = MirBuilder::new("sum3");
        let t = b.add_param(MirTy::HeapRef).unwrap();
        let x = b.ins_product_load(t, 0, MirTy::I64).unwrap();
        let y = b.ins_product_load(t, 1, MirTy::I64).unwrap();
        let z = b.ins_product_load(t, 2, MirTy::I64).unwrap();
        let s = b.ins_binop(crate::mir::MirBinOp::Add, x, y).unwrap();
        let s = b.ins_binop(crate::mir::MirBinOp::Add, s, z).unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(s)).unwrap();
        NativeBody { name: "sum3".into(), entry: Some(Label(1)), mir: b.finish().unwrap() }
    }

    /// `make3(a) = (a, a, a)` at label 2.
    fn make3() -> NativeBody {
        let mut b = MirBuilder::new("make3");
        let a = b.add_param(MirTy::I64).unwrap();
        let t = b.ins_alloc_kinds(MirAllocKind::TUPLE, vec![a, a, a], scalar3()).unwrap();
        b.set_ret_ty(MirTy::HeapRef);
        b.ret(Some(t)).unwrap();
        NativeBody { name: "make3".into(), entry: Some(Label(2)), mir: b.finish().unwrap() }
    }

    fn word_abi(params: usize, ret: MirTy) -> CallAbi {
        CallAbi { params: vec![MirTy::HeapRef; params], ret, ret_hi: None }
    }

    fn allocs(f: &MirFunc) -> usize {
        f.blocks.iter().flat_map(|b| &b.insts).filter(|i| matches!(i, MirInst::Alloc { .. })).count()
    }

    fn calls(f: &MirFunc) -> Vec<(Label, usize)> {
        f.blocks
            .iter()
            .flat_map(|b| &b.insts)
            .filter_map(|i| match i {
                MirInst::Call { target, args, .. } => Some((*target, args.len())),
                _ => None,
            })
            .collect()
    }

    fn twin(bodies: &[NativeBody], name: &str) -> Label {
        bodies.iter().find(|b| b.name == name).and_then(|b| b.entry).expect("a twin")
    }

    #[test]
    fn a_fresh_tuple_argument_goes_as_its_words() {
        let mut b = MirBuilder::new("main");
        let a = b.add_param(MirTy::I64).unwrap();
        let t = b.ins_alloc_kinds(MirAllocKind::TUPLE, vec![a, a, a], scalar3()).unwrap();
        let (r, _) = b.ins_call(Label(1), vec![t], &word_abi(1, MirTy::I64)).unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(r)).unwrap();
        let mut bodies = vec![sum3(), NativeBody { name: "main".into(), entry: Some(Label(3)), mir: b.finish().unwrap() }];
        assert_eq!(wide_call_twins(&mut bodies), 1);
        let sum = twin(&bodies, "sum3$words");
        assert_eq!(bodies.iter().find(|b| b.entry == Some(sum)).unwrap().mir.params.len(), 3);
        let main = &bodies[1].mir;
        assert_eq!(allocs(main), 0);
        assert_eq!(calls(main), vec![(sum, 3)]);
    }

    #[test]
    fn a_result_passed_on_goes_as_its_words_and_a_kept_one_stays() {
        // main(a) { k = make3(a); sum3(make3(a)); return k }
        let mut b = MirBuilder::new("main");
        let a = b.add_param(MirTy::I64).unwrap();
        let (k, _) = b.ins_call(Label(2), vec![a], &word_abi(1, MirTy::HeapRef)).unwrap();
        let (t, _) = b.ins_call(Label(2), vec![a], &word_abi(1, MirTy::HeapRef)).unwrap();
        let _ = b.ins_call(Label(1), vec![t], &word_abi(1, MirTy::I64)).unwrap();
        b.set_ret_ty(MirTy::HeapRef);
        b.ret(Some(k)).unwrap();
        let mut bodies = vec![sum3(), make3(), NativeBody { name: "main".into(), entry: Some(Label(3)), mir: b.finish().unwrap() }];
        assert_eq!(wide_call_twins(&mut bodies), 2);
        let (make, sum) = (twin(&bodies, "make3$words"), twin(&bodies, "sum3$words"));
        // `main` has no twin, so the result it returns stays the object
        // `make3` builds; the one it passes on is never built.
        let main = &bodies[2].mir;
        assert_eq!(calls(main), vec![(Label(2), 1), (make, 1), (sum, 3)]);
        assert_eq!(allocs(main), 0);
    }
    fn kinds(ks: &[u8]) -> common::PackedKinds {
        common::pack_word_kinds(ks.iter().copied())
    }

    fn main_of(mut b: MirBuilder, r: ValueId) -> NativeBody {
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(r)).unwrap();
        NativeBody { name: "main".into(), entry: Some(Label(9)), mir: b.finish().unwrap() }
    }

    fn params_of(bodies: &[NativeBody], name: &str) -> usize {
        bodies.iter().find(|b| b.name == name).expect("a twin").mir.params.len()
    }

    #[test]
    fn a_nested_tuple_argument_goes_as_its_leaf_words() {
        // weigh(t) = t.0.0 + t.0.1 + t.1, called with ((a, a), a).
        let mut b = MirBuilder::new("weigh");
        let t = b.add_param(MirTy::HeapRef).unwrap();
        let p = b.ins_product_load(t, 0, MirTy::HeapRef).unwrap();
        let x = b.ins_product_load(p, 0, MirTy::I64).unwrap();
        let y = b.ins_product_load(p, 1, MirTy::I64).unwrap();
        let z = b.ins_product_load(t, 1, MirTy::I64).unwrap();
        let s = b.ins_binop(crate::mir::MirBinOp::Add, x, y).unwrap();
        let s = b.ins_binop(crate::mir::MirBinOp::Add, s, z).unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(s)).unwrap();
        let weigh = NativeBody { name: "weigh".into(), entry: Some(Label(1)), mir: b.finish().unwrap() };
        let mut m = MirBuilder::new("main");
        let a = m.add_param(MirTy::I64).unwrap();
        let inner = m.ins_alloc_kinds(MirAllocKind::TUPLE, vec![a, a], kinds(&[common::WORD_SCALAR; 2])).unwrap();
        let outer = m.ins_alloc_kinds(MirAllocKind::TUPLE, vec![inner, a], kinds(&[common::WORD_POINTER, common::WORD_SCALAR])).unwrap();
        let (r, _) = m.ins_call(Label(1), vec![outer], &word_abi(1, MirTy::I64)).unwrap();
        let mut bodies = vec![weigh, main_of(m, r)];
        assert_eq!(wide_call_twins(&mut bodies), 1);
        assert_eq!(params_of(&bodies, "weigh$words"), 3);
        assert_eq!(allocs(&bodies[1].mir), 0);
        assert_eq!(allocs(&bodies.iter().find(|b| b.name == "weigh$words").unwrap().mir), 0);
    }

    #[test]
    fn a_heap_item_goes_as_a_word_beside_the_numbers() {
        // len3(t) = len(t.0) + t.1 + t.2, called with (s, a, a).
        let mut b = MirBuilder::new("len3");
        let t = b.add_param(MirTy::HeapRef).unwrap();
        let s = b.ins_product_load(t, 0, MirTy::HeapRef).unwrap();
        let n = b.ins_array_len(s).unwrap();
        let y = b.ins_product_load(t, 1, MirTy::I64).unwrap();
        let z = b.ins_product_load(t, 2, MirTy::I64).unwrap();
        let r = b.ins_binop(crate::mir::MirBinOp::Add, n, y).unwrap();
        let r = b.ins_binop(crate::mir::MirBinOp::Add, r, z).unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(r)).unwrap();
        let len3 = NativeBody { name: "len3".into(), entry: Some(Label(1)), mir: b.finish().unwrap() };
        let mut m = MirBuilder::new("main");
        let a = m.add_param(MirTy::I64).unwrap();
        let s = m.add_param(MirTy::HeapRef).unwrap();
        let k = kinds(&[common::WORD_POINTER, common::WORD_SCALAR, common::WORD_SCALAR]);
        let tup = m.ins_alloc_kinds(MirAllocKind::TUPLE, vec![s, a, a], k).unwrap();
        let (r, _) = m.ins_call(Label(1), vec![tup], &word_abi(1, MirTy::I64)).unwrap();
        let mut bodies = vec![len3, main_of(m, r)];
        assert_eq!(wide_call_twins(&mut bodies), 1);
        let twin = bodies.iter().find(|b| b.name == "len3$words").unwrap();
        assert_eq!(twin.mir.params.len(), 3);
        assert_eq!(twin.mir.ty(twin.mir.params[0]), MirTy::HeapRef);
        assert_eq!(allocs(&bodies[1].mir), 0);
    }

    #[test]
    fn an_option_argument_goes_as_its_words_and_a_flag() {
        // first(o) = o.0 (read past a `Some` test), called with a `Some`
        // of a fresh tuple and with a `None`.
        let mut b = MirBuilder::new("first");
        let o = b.add_param(MirTy::NicheOpt).unwrap();
        let _ = b.ins_not(o).unwrap();
        let t = b.ins_cast(MirCastKind::Word, MirTy::HeapRef, o).unwrap();
        let x = b.ins_product_load(t, 0, MirTy::I64).unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(x)).unwrap();
        let first = NativeBody { name: "first".into(), entry: Some(Label(1)), mir: b.finish().unwrap() };
        let mut m = MirBuilder::new("main");
        let a = m.add_param(MirTy::I64).unwrap();
        let tup = m.ins_alloc_kinds(MirAllocKind::TUPLE, vec![a, a, a], scalar3()).unwrap();
        let some = m.ins_cast(MirCastKind::Word, MirTy::NicheOpt, tup).unwrap();
        let niche_abi = CallAbi { params: vec![MirTy::NicheOpt], ret: MirTy::I64, ret_hi: None };
        let (r1, _) = m.ins_call(Label(1), vec![some], &niche_abi).unwrap();
        let zero = m.ins_const(MirConst::I64(0)).unwrap();
        let none = m.ins_cast(MirCastKind::Word, MirTy::NicheOpt, zero).unwrap();
        let (r2, _) = m.ins_call(Label(1), vec![none], &niche_abi).unwrap();
        let r = m.ins_binop(crate::mir::MirBinOp::Add, r1, r2).unwrap();
        let mut bodies = vec![first, main_of(m, r)];
        assert_eq!(wide_call_twins(&mut bodies), 2);
        assert_eq!(params_of(&bodies, "first$words"), 4);
        assert_eq!(allocs(&bodies[1].mir), 0);
    }

    #[test]
    fn a_result_argument_goes_as_its_words_and_a_flag() {
        // first(r) = r.0 + (r & 1) + len(r ^ 1): an `Ok`'s item, its tag
        // and an `Err`'s payload, called with an `Ok` of a fresh tuple and
        // with an `Err` of a string.
        let mut b = MirBuilder::new("first");
        let r = b.add_param(MirTy::NicheRes).unwrap();
        let one = b.ins_const(MirConst::I64(1)).unwrap();
        let bit = b.ins_binop(MirBinOp::BitAnd, r, one).unwrap();
        let t = b.ins_cast(MirCastKind::Word, MirTy::HeapRef, r).unwrap();
        let x = b.ins_product_load(t, 0, MirTy::I64).unwrap();
        let e = b.ins_binop(MirBinOp::Xor, r, one).unwrap();
        let e = b.ins_cast(MirCastKind::Word, MirTy::HeapRef, e).unwrap();
        let n = b.ins_array_len(e).unwrap();
        let s = b.ins_binop(MirBinOp::Add, x, bit).unwrap();
        let s = b.ins_binop(MirBinOp::Add, s, n).unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(s)).unwrap();
        let first = NativeBody { name: "first".into(), entry: Some(Label(1)), mir: b.finish().unwrap() };
        let mut m = MirBuilder::new("main");
        let a = m.add_param(MirTy::I64).unwrap();
        let text = m.add_param(MirTy::HeapRef).unwrap();
        let tup = m.ins_alloc_kinds(MirAllocKind::TUPLE, vec![a, a, a], scalar3()).unwrap();
        let ok = m.ins_cast(MirCastKind::Word, MirTy::NicheRes, tup).unwrap();
        let res_abi = CallAbi { params: vec![MirTy::NicheRes], ret: MirTy::I64, ret_hi: None };
        let (r1, _) = m.ins_call(Label(1), vec![ok], &res_abi).unwrap();
        let one = m.ins_const(MirConst::I64(1)).unwrap();
        let err = m.ins_binop(MirBinOp::BitOr, text, one).unwrap();
        let (r2, _) = m.ins_call(Label(1), vec![err], &res_abi).unwrap();
        let r = m.ins_binop(MirBinOp::Add, r1, r2).unwrap();
        let mut bodies = vec![first, main_of(m, r)];
        assert_eq!(wide_call_twins(&mut bodies), 2);
        // Three `Ok` words, one shared with the `Err` payload, and a flag.
        assert_eq!(params_of(&bodies, "first$words"), 4);
        assert_eq!(allocs(&bodies[1].mir), 0);
        let twin = bodies.iter().find(|b| b.name == "first$words").unwrap();
        twin.mir.verify().unwrap();
        bodies[1].mir.verify().unwrap();
    }
}
