//! Wide direct calls: a small object of immediate words (an arity-3 or
//! arity-4 tuple of scalars, or for a result an `Option` of a tuple of
//! scalars, which is a niche word over a tuple object) crosses a direct
//! call as its three or four words when neither side needs the object.
//!
//! A body gets a twin, `name$words`, that takes such a parameter as its
//! words (one MIR parameter per word) and returns such a result as its
//! words (`Return { lo, hi, more }`), and each direct call that can hand
//! over and take the words calls the twin instead. So neither side builds
//! the object: not the caller's argument, not the callee's result.
//!
//! What "can" means is one question per value of a body (a *node*: a
//! value with its `Word` casts, safepoint barriers and copy-on-write
//! shares): whether its words
//! are known where it is made (a fresh `alloc.tuple` of immediates, a
//! twin's parameter, a switched call's result) and every read of it takes
//! words (an item load, a `Some` test, an argument the twin takes as
//! words, the twin's return). The answers depend on each other across
//! calls, so they are a greatest fixpoint over the whole program: start
//! from "every node dissolves", drop what breaks, repeat.
//!
//! The body itself stays for every other caller (function values,
//! dictionaries, the runtime's entries, calls that cannot pass words).
//! Runs on the captured bodies before counts are inserted
//! ([`super::native_capture`]), so the object's counts never appear.

use std::collections::{HashMap, HashSet};

use super::cse::dce;
use super::func::MirFunc;
use super::inst::{MirAllocKind, MirCastKind, MirConst, MirInst, MirUnaryOp, Terminator, ValueId, MAX_RET_MORE};
use super::native_capture::NativeBody;
use super::ty::MirTy;
use crate::bodies::Label;

fn immediate(t: MirTy) -> bool {
    matches!(t, MirTy::I64 | MirTy::MaybeObj | MirTy::F64 | MirTy::Bool)
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

/// `v` through `Word` casts to the value they reinterpret.
fn through_casts(f: &MirFunc, mut v: ValueId) -> ValueId {
    while let Some(MirInst::Cast { kind: MirCastKind::Word, src, .. }) = f.def_inst(v) {
        v = *src;
    }
    v
}

/// Where a node's object comes from.
#[derive(Clone, Debug)]
enum Src {
    /// A fresh tuple of immediates, each a scalar by its kind.
    Alloc { items: Vec<ValueId>, kind: MirAllocKind, kinds: common::PackedKinds },
    /// Parameter `j` of the body.
    Param(usize),
    /// The result of call `c` (an index into [`Body::calls`]).
    Call(usize),
    /// A φ of other nodes' objects: its incoming values, by block.
    Phi(Vec<(super::inst::BlockId, ValueId)>),
    /// Anything else (another kind of tuple, a tuple of heap words).
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
    /// Per node: it, or a node a φ joins it with, meets a call edge (a
    /// parameter, a call's result or argument, a return). Others are
    /// `forward_tuple_loads`' to take apart, not this pass's.
    touches: Vec<bool>,
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
    matches!(t, MirTy::HeapRef | MirTy::NicheOpt)
}

fn read_body(name: &str, f: &MirFunc, by_label: &HashMap<Label, usize>) -> Body {
    let twin = twinnable(name, f);
    let mut nodes: Vec<Node> = Vec::new();
    let mut member: HashMap<ValueId, usize> = HashMap::new();
    let mut calls: Vec<CallSite> = Vec::new();
    let mut call_of: HashMap<ValueId, usize> = HashMap::new();
    let mut params = vec![None; f.params.len()];
    let add = |root: ValueId, src: Src, nodes: &mut Vec<Node>, member: &mut HashMap<ValueId, usize>| {
        member.insert(root, nodes.len());
        nodes.push(Node { members: vec![root], src, uses: Vec::new(), other: false, niche: f.ty(root) == MirTy::NicheOpt });
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
                    let words = elems.len() < common::PACKED_KIND_WORDS
                        && elems.iter().enumerate().all(|(k, e)| immediate(f.ty(*e)) && common::packed_word_kind(*kinds, k) == common::WORD_SCALAR);
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
                _ => {}
            }
        }
    }
    // Casts and barriers join their source's node, to a fixed point (a cast
    // of a cast).
    loop {
        let mut grew = false;
        for i in f.blocks.iter().flat_map(|b| &b.insts) {
            let (dest, src) = match i {
                MirInst::Cast { kind: MirCastKind::Word, dest, src, .. } => (*dest, *src),
                MirInst::GcBarrier { dest, object, .. } => (*dest, *object),
                // A share of an object nothing writes (a tuple) is its word.
                MirInst::Cow { dest, src, unique: false, field: None } => (*dest, *src),
                _ => continue,
            };
            if let Some(&n) = member.get(&src)
                && !member.contains_key(&dest)
            {
                member.insert(dest, n);
                nodes[n].members.push(dest);
                nodes[n].niche |= f.ty(dest) == MirTy::NicheOpt;
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
                MirInst::ProductLoad { dest, object, slot } if member.contains_key(object) => {
                    nodes[member[object]].uses.push(Use::Load { dest: *dest, slot: *slot as usize });
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
    let touches = touches(&nodes);
    Body { nodes, member, calls, rets, params, twinnable: twin, wide_ret, touches }
}

/// [`Body::touches`]: each node's φ-joined group meets a call edge.
fn touches(nodes: &[Node]) -> Vec<bool> {
    let mut group: Vec<usize> = (0..nodes.len()).collect();
    fn find(g: &mut [usize], mut x: usize) -> usize {
        while g[x] != x {
            g[x] = g[g[x]];
            x = g[x];
        }
        x
    }
    for (n, node) in nodes.iter().enumerate() {
        for u in &node.uses {
            if let Use::Phi(p) = *u {
                let (a, b) = (find(&mut group, n), find(&mut group, p));
                group[a] = b;
            }
        }
    }
    let mut hit = vec![false; nodes.len()];
    for (n, node) in nodes.iter().enumerate() {
        if matches!(node.src, Src::Param(_) | Src::Call(_)) || node.uses.iter().any(|u| matches!(u, Use::Arg { .. } | Use::Ret)) {
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

/// A class of [`Key`]s that must agree on their words.
#[derive(Clone, Debug, Default)]
struct Class {
    items: Option<Vec<MirTy>>,
    kind: Option<MirAllocKind>,
    kinds: common::PackedKinds,
    niche: Option<bool>,
    bad: bool,
}

impl Class {
    fn words(&self) -> Option<Vec<MirTy>> {
        if self.bad {
            return None;
        }
        let mut w = self.items.clone()?;
        if self.niche == Some(true) {
            w.push(MirTy::Bool);
        }
        (3..=2 + MAX_RET_MORE).contains(&w.len()).then_some(w)
    }

    fn set_niche(&mut self, niche: bool) {
        match self.niche {
            Some(n) if n != niche => self.bad = true,
            _ => self.niche = Some(niche),
        }
    }
}

struct Classes {
    parent: Vec<usize>,
    index: HashMap<Key, usize>,
}

impl Classes {
    fn id(&mut self, k: Key) -> usize {
        if let Some(&i) = self.index.get(&k) {
            return i;
        }
        let n = self.parent.len();
        self.parent.push(n);
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

    fn union(&mut self, a: Key, b: Key) {
        let (a, b) = (self.id(a), self.id(b));
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.parent[a] = b;
        }
    }

    fn of(&mut self, k: Key) -> usize {
        let i = self.id(k);
        self.find(i)
    }
}

/// The two ways a body runs: as itself (its parameters and result are
/// objects) and as its twin.
const ORIG: usize = 0;
const TWIN: usize = 1;

struct Program<'a> {
    bodies: &'a [NativeBody],
    info: Vec<Body>,
    /// Each node's class's words, by body and node (`None`: never words).
    words: Vec<Vec<Option<Vec<MirTy>>>>,
    /// Each node's class is an `Option`'s.
    niche: Vec<Vec<bool>>,
    param_words: Vec<Vec<Option<Vec<MirTy>>>>,
    ret_words: Vec<Option<Vec<MirTy>>>,
    ret_niche: Vec<bool>,
    /// The allocation a rebuilt result makes: its class's tuple kind and
    /// word kinds.
    ret_alloc: Vec<(MirAllocKind, common::PackedKinds)>,
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
        // Shapes: unify every edge words could cross.
        let mut cls = Classes { parent: Vec::new(), index: HashMap::new() };
        for (bi, b) in info.iter().enumerate() {
            for (n, node) in b.nodes.iter().enumerate() {
                let k = Key::Node(bi, n);
                cls.id(k);
                if let Src::Param(j) = node.src {
                    cls.union(k, Key::Param(bi, j));
                }
                if let Src::Call(c) = node.src
                    && let Some(t) = b.calls[c].callee
                {
                    cls.union(k, Key::Ret(t));
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
        }
        let mut classes: HashMap<usize, Class> = HashMap::new();
        for (bi, b) in info.iter().enumerate() {
            let f = &bodies[bi].mir;
            for (n, node) in b.nodes.iter().enumerate() {
                let c = cls.of(Key::Node(bi, n));
                let class = classes.entry(c).or_default();
                class.set_niche(node.niche);
                match &node.src {
                    Src::Alloc { items, kind, kinds } => {
                        let tys: Vec<MirTy> = items.iter().map(|e| f.ty(*e)).collect();
                        match &class.items {
                            Some(prev) if *prev != tys => class.bad = true,
                            Some(_) => {}
                            None => {
                                class.items = Some(tys);
                                class.kind = Some(*kind);
                                class.kinds = *kinds;
                            }
                        }
                    }
                    Src::Opaque => class.bad = true,
                    _ => {}
                }
            }
            if b.wide_ret {
                let c = cls.of(Key::Ret(bi));
                classes.entry(c).or_default().set_niche(f.ret_ty == Some(MirTy::NicheOpt));
            }
        }
        // Item loads must read an item of its type.
        for (bi, b) in info.iter().enumerate() {
            let f = &bodies[bi].mir;
            for (n, node) in b.nodes.iter().enumerate() {
                let c = cls.of(Key::Node(bi, n));
                let class = classes.entry(c).or_default();
                for u in &node.uses {
                    if let Use::Load { dest, slot } = u {
                        let ok = class.items.as_ref().is_some_and(|items| items.get(*slot) == Some(&f.ty(*dest)));
                        if !ok {
                            class.bad = true;
                        }
                    }
                }
            }
        }
        let ret_alloc: Vec<(MirAllocKind, common::PackedKinds)> = (0..info.len())
            .map(|bi| {
                let c = cls.of(Key::Ret(bi));
                classes.get(&c).and_then(|cl| Some((cl.kind?, cl.kinds))).unwrap_or((MirAllocKind::TUPLE, 0))
            })
            .collect();
        let words_of = |k: Key, cls: &mut Classes| -> (Option<Vec<MirTy>>, bool) {
            let c = cls.of(k);
            classes.get(&c).map_or((None, false), |cl| (cl.words(), cl.niche == Some(true)))
        };
        let mut words = Vec::new();
        let mut niche = Vec::new();
        let mut param_words: Vec<Vec<Option<Vec<MirTy>>>> = Vec::new();
        let mut ret_words = Vec::new();
        let mut ret_niche = Vec::new();
        for (bi, b) in info.iter().enumerate() {
            let (w, ni): (Vec<_>, Vec<_>) = (0..b.nodes.len()).map(|n| words_of(Key::Node(bi, n), &mut cls)).unzip();
            words.push(w);
            niche.push(ni);
            param_words.push(
                (0..b.params.len())
                    .map(|j| match b.params[j] {
                        // A niche parameter would need a branch to split or
                        // rebuild: it stays one word.
                        Some(_) if b.twinnable => {
                            let (w, ni) = words_of(Key::Param(bi, j), &mut cls);
                            w.filter(|_| !ni)
                        }
                        _ => None,
                    })
                    .collect(),
            );
            let (w, ni) = if b.wide_ret { words_of(Key::Ret(bi), &mut cls) } else { (None, false) };
            ret_words.push(w);
            ret_niche.push(ni);
        }
        let param_w = param_words.iter().map(|p| p.iter().map(Option::is_some).collect()).collect();
        let ret_w = ret_words.iter().map(Option::is_some).collect();
        let viable = [0, 1].map(|_| words.iter().map(|w| w.iter().map(Option::is_some).collect()).collect());
        let switch = [0, 1].map(|_| info.iter().map(|b| vec![true; b.calls.len()]).collect());
        Program { bodies, info, words, niche, param_words, ret_words, ret_niche, ret_alloc, param_w, ret_w, viable, switch }
    }

    fn has_twin(&self, t: usize) -> bool {
        self.ret_w[t] || self.param_w[t].iter().any(|&w| w)
    }

    /// Whether node `n` of body `b` keeps no object when `b` runs as `ctx`.
    fn node_ok(&self, b: usize, ctx: usize, n: usize) -> bool {
        let node = &self.info[b].nodes[n];
        if node.other || self.words[b][n].is_none() {
            return false;
        }
        if !self.info[b].touches[n] {
            return false;
        }
        let src = match node.src {
            Src::Alloc { .. } => true,
            Src::Param(j) => ctx == TWIN && self.param_w[b][j],
            Src::Call(c) => self.switch[ctx][b][c] && self.info[b].calls[c].callee.is_some_and(|t| self.ret_w[t]),
            Src::Phi(ref args) => args.iter().all(|(_, a)| self.info[b].member.get(a).is_some_and(|&m| self.viable[ctx][b][m])),
            Src::Opaque => false,
        };
        src && node.uses.iter().all(|u| match *u {
            Use::Load { .. } => true,
            Use::Test => self.niche[b][n],
            Use::Arg { call, j } => {
                self.switch[ctx][b][call] && self.info[b].calls[call].callee.is_some_and(|t| self.param_w[t].get(j) == Some(&true))
            }
            Use::Ret => ctx == TWIN && self.ret_w[b],
            Use::Phi(p) => self.viable[ctx][b][p],
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
                // A caller that keeps the object rebuilds it from the
                // words, past the call; an `Option` would need a branch.
                if self.ret_niche[t] {
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
        !info.rets.is_empty()
            && info.rets.iter().any(|(_, r)| matches!(r, RetSite::Node(_)))
            && info.rets.iter().all(|&(_, r)| match r {
                RetSite::Node(n) => self.viable[TWIN][b][n],
                RetSite::None => self.ret_niche[b],
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

    /// Body `b` running as `ctx`, rewritten: its dissolved nodes' objects
    /// gone, its switched calls entering twins (`twin_of`).
    fn rewrite(&self, b: usize, ctx: usize, twin_of: &HashMap<usize, Label>) -> (MirFunc, usize) {
        let mut f = self.bodies[b].mir.clone();
        let info = &self.info[b];
        // Constants a rewrite adds: in place of a dissolved tuple, and at
        // the end of a block.
        let mut at: HashMap<ValueId, Vec<MirInst>> = HashMap::new();
        let mut tail: HashMap<usize, Vec<MirInst>> = HashMap::new();
        // Each dissolved node's words.
        let mut node_words: HashMap<usize, Vec<ValueId>> = HashMap::new();
        let mut gone: HashSet<ValueId> = HashSet::new();
        let mut subst: HashMap<ValueId, ValueId> = HashMap::new();
        if ctx == TWIN {
            let mut params = Vec::new();
            for (j, &p) in f.params.clone().iter().enumerate() {
                if self.param_w[b][j] {
                    let tys = self.param_words[b][j].clone().expect("a parameter's words");
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
                let tys = self.ret_words[t].clone().expect("a result's words");
                let dissolved = info.member.get(&call.dest).copied().filter(|&n| self.viable[ctx][b][n]);
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
        // A dissolved φ's words are φs of its incoming nodes' words.
        for (n, node) in info.nodes.iter().enumerate() {
            if self.viable[ctx][b][n]
                && let Src::Phi(_) = node.src
            {
                let tys = self.words[b][n].clone().expect("a φ's words");
                let ws = tys.iter().map(|&t| new_value(&mut f, t)).collect();
                node_words.insert(n, ws);
            }
        }
        for (n, node) in info.nodes.iter().enumerate() {
            if !self.viable[ctx][b][n] {
                continue;
            }
            gone.extend(node.members.iter().copied());
            if let Src::Alloc { items, .. } = &node.src {
                let mut ws = items.clone();
                if self.niche[b][n] {
                    // `Some`'s flag: where the tuple was made, or, read by
                    // nothing but a return, at the end of its block.
                    let t = new_value(&mut f, MirTy::Bool);
                    let flag = MirInst::Const { dest: t, c: MirConst::Bool(true) };
                    let ret = info.rets.iter().find(|(_, r)| matches!(r, RetSite::Node(m) if *m == n)).map(|(bi, _)| *bi);
                    match ret.filter(|_| !node.uses.iter().any(|u| matches!(u, Use::Test))) {
                        Some(bi) => tail.entry(bi).or_default().push(flag),
                        None => at.entry(node.members[0]).or_default().push(flag),
                    }
                    ws.push(t);
                }
                node_words.insert(n, ws);
            }
            for u in &node.uses {
                if let Use::Load { dest, slot } = u {
                    subst.insert(*dest, node_words[&n][*slot]);
                    gone.insert(*dest);
                }
            }
        }
        for (n, node) in info.nodes.iter().enumerate() {
            if self.viable[ctx][b][n]
                && let Src::Phi(args) = &node.src
            {
                let tys = self.words[b][n].clone().expect("a φ's words");
                let phis = (0..tys.len())
                    .map(|k| MirInst::Phi { dest: node_words[&n][k], ty: tys[k], args: args.iter().map(|&(blk, a)| (blk, node_words[&info.member[&a]][k])).collect() })
                    .collect::<Vec<_>>();
                at.entry(node.members[0]).or_default().extend(phis);
            }
        }
        let flag_of = |v: ValueId| -> Option<ValueId> {
            let n = *info.member.get(&v)?;
            self.viable[ctx][b][n].then(|| *node_words[&n].last().expect("words"))
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
                let mut rebuild = None;
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
                        if !info.member.get(&old).is_some_and(|&n| self.viable[ctx][b][n]) {
                            // The caller keeps the object: build it here.
                            let (kind, kinds) = self.ret_alloc[t];
                            rebuild = Some(MirInst::Alloc { dest: old, kind, elems: ws.clone(), kinds });
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
            let tys = self.ret_words[b].clone().expect("a result's words");
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
/// those calls at it. Returns how many calls moved.
pub fn wide_call_twins(bodies: &mut Vec<NativeBody>) -> usize {
    let mut prog = Program::new(bodies);
    if !(0..prog.info.len()).any(|b| prog.has_twin(b)) {
        return 0;
    }
    prog.solve();
    let need = prog.twins();
    if need.is_empty() {
        return 0;
    }
    let first = bodies.iter().flat_map(|b| b.entry.into_iter().chain(b.mir.code_targets())).map(|l| l.0).max().unwrap_or(0) + 1;
    let twin_of: HashMap<usize, Label> = need.iter().zip(first..).map(|(&t, l)| (t, Label(l))).collect();
    let mut moved = 0;
    let mut rewritten: Vec<(usize, MirFunc)> = Vec::new();
    for b in 0..bodies.len() {
        if prog.switch[ORIG][b].iter().any(|&s| s) {
            let (f, n) = prog.rewrite(b, ORIG, &twin_of);
            moved += n;
            rewritten.push((b, f));
        }
    }
    let mut twins = Vec::new();
    for &t in &need {
        let (mut f, n) = prog.rewrite(t, TWIN, &twin_of);
        moved += n;
        let name = format!("{}$words", bodies[t].name);
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
}
