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
//! The pass is deliberately plain: no inc/dec pairs are cancelled and no
//! ownership moves into calls yet (elision comes next). What it leaves as
//! leaks, which the oracle reports but which free nothing early: a tail
//! call's arguments (decrementing them after the call would end the tail
//! call), a frame that unwinds through a panic, a coroutine abandoned
//! before it finishes.

use std::collections::{HashMap, HashSet};

use super::frame_words::{barrier_pairs, heap_word, opaque_words, pair_words, pointer_words, ty_of};
use super::func::{MirBlock, MirFunc, is_tail_call_inst};
use super::inst::{BlockId, MirCastKind, MirInst, Terminator, ValueId};
use super::ty::MirTy;

/// Whether this compile inserts the ops: a run that keeps counts
/// (`COIL_RC_CHECK`, as `machine::rc_check_mode` reads it).
pub fn rc_insert_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("COIL_RC_CHECK").is_ok_and(|v| !v.is_empty() && v != "0"))
}

/// Insert the count ops into `func` (and its cleanup pad). Returns how many
/// it inserted.
pub fn insert_rc(func: &mut MirFunc) -> usize {
    let mut n = 0;
    if let Some(pad) = func.cleanup.as_deref_mut() {
        n += insert_rc(pad);
    }
    // Counts free what an early free would (`hir/early_free.rs`): its
    // frees go, or a count would be decremented on a freed object.
    for inst in func.blocks.iter_mut().flat_map(|b| b.insts.iter_mut()) {
        if let MirInst::FreeEarly { dest, .. } = *inst {
            *inst = MirInst::Const { dest, c: super::inst::MirConst::Bool(false) };
        }
    }
    n + Pass::new(func).run(func)
}

struct Pass {
    /// Each counted value's root (itself for a root).
    base: HashMap<ValueId, ValueId>,
    /// Roots that are borrowed (parameters).
    borrowed: HashSet<ValueId>,
    /// Owned roots whose definition already owns its word.
    owned_at_def: HashSet<ValueId>,
}

/// The value an alias-defining instruction names the same object as.
fn alias_source(inst: &MirInst, counted: &dyn Fn(ValueId) -> bool) -> Option<ValueId> {
    match inst {
        MirInst::Cast { kind: MirCastKind::Word, src, .. } | MirInst::Unary { src, .. } => counted(*src).then_some(*src),
        MirInst::Cast { src, .. } => counted(*src).then_some(*src),
        MirInst::Bin { lhs, rhs, .. } => [*lhs, *rhs].into_iter().find(|v| counted(*v)),
        MirInst::HeapFieldStore { value, .. } | MirInst::StoreIndex { value, .. } => counted(*value).then_some(*value),
        MirInst::ArrayPush { array, .. } | MirInst::LanePush { array, .. } | MirInst::LanesToWords { array, .. } => counted(*array).then_some(*array),
        _ => None,
    }
}

impl Pass {
    fn new(func: &MirFunc) -> Self {
        let pointers = pointer_words(func);
        let mut counted: HashSet<ValueId> = (0..func.types.len() as u32).map(ValueId).filter(|v| matches!(ty_of(func, *v), t if heap_word(t) || t == MirTy::Value)).collect();
        counted.extend(opaque_words(func));
        counted.extend(pair_words(func, &pointers));
        counted.extend(pointers);
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
                | MirInst::Yield { .. } => counted.extend(inst.dests().filter(|d| maybe(*d))),
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
        let borrowed: HashSet<ValueId> = if generator { HashSet::new() } else { func.params.iter().copied().filter(|p| counted.contains(p)).collect() };
        for &p in func.params.iter().filter(|p| counted.contains(p)) {
            base.insert(p, p);
            if generator {
                owned_at_def.insert(p);
            }
        }
        Self { base, borrowed, owned_at_def }
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

    fn run(&self, func: &mut MirFunc) -> usize {
        if self.base.is_empty() {
            return 0;
        }
        let live_in = self.live_in(func);
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
                // A static or a home keeps its word as long as the program
                // (a later store's old word is not released yet: a leak).
                if let MirInst::StoreStatic { value, .. } | MirInst::HomeStore { value, .. } = inst
                    && (self.root(*value).is_some() || matches!(ty_of(func, *value), MirTy::I64 | MirTy::Value))
                {
                    ops.insert(0, MirInst::RcInc { dest: tok(&mut new_types), src: *value, guard: None });
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
                                if counted || matches!(ty_of(func, v), MirTy::I64 | MirTy::Value) {
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
                                if self.root(p) == Some(p) && (into.contains(&p) || args.contains(&p)) {
                                    ops.push(MirInst::RcInc { dest: tok(&mut new_types), src: p, guard: None });
                                }
                            }
                        }
                        // Each φ arg: moved when its root dies on this edge
                        // (once), else incremented.
                        let mut moved: HashSet<ValueId> = HashSet::new();
                        for (v, _) in phi_args_from(sb, b) {
                            match self.owned(v) {
                                // A bound payload was incremented above when it
                                // flows on: the φ takes that owner.
                                Some(r) if !into.contains(&r) && moved.insert(r) => {}
                                _ if self.root(v).is_none() && !matches!(ty_of(func, v), MirTy::I64 | MirTy::Value) => {}
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
            if after[bi].is_empty() && before_term[bi].is_empty() {
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
        inserted
    }
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
