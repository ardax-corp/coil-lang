//! Loop-invariant code motion on MIR (COI-280).
//!
//! Runs after SSA lower, local CSE and copy forwarding, on dense and LIR
//! bodies alike. An instruction whose operands the loop does not define
//! moves to a preheader when computing it early changes nothing: consts,
//! float and bitwise arithmetic, compares, casts, pure host math, a field
//! of an object nothing in the loop writes, the length of an array
//! nothing in the loop resizes.
//!
//! An instruction that may trap (int arithmetic that can overflow, a
//! division by a variable, an element read, a call of a pure function)
//! moves only where the loop is sure to compute it: in the header ahead of
//! anything observable, or in a block every iteration runs before it can
//! leave, behind a copy of the header's cheap test so a loop that runs no
//! iteration computes nothing. Float divide is IEEE and moves freely.

use std::collections::HashSet;

use super::cse::dce;
use super::func::MirFunc;
use super::inst::{BlockId, MirBinOp, MirInst, Terminator, ValueId};

/// Hoist loop-invariant instructions. Returns how many moved. `dense`: a
/// literal and a heap value may move too; LIR keeps a literal where it is
/// an immediate operand, and every heap word in place.
pub fn licm(func: &mut MirFunc, dense: bool) -> usize {
    let mut total = 0;
    for _ in 0..func.blocks.len().saturating_mul(4).max(4) {
        let loops = natural_loops(func);
        if loops.is_empty() {
            break;
        }
        let mut progressed = 0;
        let mut loops = loops;
        loops.sort_by_key(|lp| lp.blocks.len());
        for lp in loops {
            progressed += hoist_loop(func, &lp, dense);
            if progressed > 0 {
                break;
            }
        }
        total += progressed;
        if progressed == 0 {
            break;
        }
    }
    if total > 0 {
        dce(func);
    }
    total
}

/// Mark the reads of arrays no code ever writes as invariant
/// ([`MirFunc::invariant_loads`], #1561): a `LoadStatic` of one of
/// `slots` (a module `const` table: its slot is set once at startup), the
/// values already marked as such an array (a local table built once), a
/// φ of only those, and every element read and length of one of them.
pub fn mark_frozen_arrays(func: &mut MirFunc, slots: &HashSet<u32>) {
    let mut frozen: HashSet<ValueId> = HashSet::new();
    for inst in func.blocks.iter().flat_map(|b| &b.insts) {
        match inst {
            MirInst::LoadStatic { dest, slot } if slots.contains(slot) => {
                frozen.insert(*dest);
            }
            MirInst::Phi { dest, .. } if func.invariant_loads.contains(dest) => {
                frozen.insert(*dest);
            }
            _ => {}
        }
    }
    if frozen.is_empty() {
        return;
    }
    loop {
        let mut grew = false;
        for inst in func.blocks.iter().flat_map(|b| &b.insts) {
            if let MirInst::Phi { dest, args, .. } = inst
                && !frozen.contains(dest)
                && args.iter().all(|(_, v)| v == dest || frozen.contains(v))
            {
                frozen.insert(*dest);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    for inst in func.blocks.iter().flat_map(|b| &b.insts) {
        match inst {
            MirInst::Index { dest, array, .. } | MirInst::ArrayLen { dest, array } if frozen.contains(array) => {
                func.invariant_loads.insert(*dest);
            }
            _ => {}
        }
    }
    func.invariant_loads.extend(frozen);
}

pub(super) struct LoopInfo {
    pub header: BlockId,
    pub blocks: HashSet<BlockId>,
}

pub(super) fn natural_loops(func: &MirFunc) -> Vec<LoopInfo> {
    let n = func.blocks.len();
    if n == 0 {
        return Vec::new();
    }
    let preds = func.preds();
    let dom = Dominators::new(func, &preds);
    let mut by_header: Vec<HashSet<BlockId>> = vec![HashSet::new(); n];
    for b in &func.blocks {
        let Some(term) = &b.term else {
            continue;
        };
        for s in term.succs() {
            if s.index() >= n {
                continue;
            }
            if !dom.dominates(s, b.id) {
                continue;
            }
            let body = loop_body(&preds, b.id, s);
            if by_header[s.index()].is_empty() {
                by_header[s.index()] = body;
            } else {
                by_header[s.index()].extend(body);
            }
        }
    }
    by_header
        .into_iter()
        .enumerate()
        .filter_map(|(i, blocks)| {
            if blocks.is_empty() {
                None
            } else {
                Some(LoopInfo {
                    header: BlockId(i as u32),
                    blocks,
                })
            }
        })
        .collect()
}

#[cfg(test)]
thread_local! {
    /// Steps the dominator computation took, for the bound of #1518.
    static DOM_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Dominance over a body's blocks, from its immediate-dominator tree
/// (Cooper, Harvey and Kennedy, "A Simple, Fast Dominance Algorithm").
///
/// The roots are the entry and every other block nothing branches to; a
/// block no root reaches (a cycle nothing enters) is dominated by every
/// block. That is the largest solution of `dom(b) = {b} ∪ ⋂ dom(preds)`,
/// which the set-per-block fixpoint this replaced computed (#1518).
pub(super) struct Dominators {
    /// Each block's parent in the tree; `None` for a root and for a block
    /// no root reaches.
    idom: Vec<Option<BlockId>>,
    /// Each block's span in a preorder walk of the tree: `d` dominates `b`
    /// when `b`'s span lies inside `d`'s. `(0, 0)` when no root reaches it.
    span: Vec<(u32, u32)>,
    /// Whether a root reaches the block.
    reached: Vec<bool>,
}

impl Dominators {
    pub(super) fn new(func: &MirFunc, preds: &[Vec<BlockId>]) -> Self {
        let n = func.blocks.len();
        if n == 0 {
            return Dominators { idom: Vec::new(), span: Vec::new(), reached: Vec::new() };
        }
        // Node `n` is a virtual root above the entry and every block
        // nothing branches to.
        let root = n;
        let succs = |b: usize| -> Vec<usize> {
            if b == root {
                let mut out = vec![func.entry.index()];
                out.extend((0..n).filter(|&i| i != func.entry.index() && preds[i].is_empty()));
                out
            } else {
                func.blocks[b]
                    .term
                    .as_ref()
                    .map(|t| t.succs().into_iter().map(|s| s.index()).filter(|&s| s < n).collect())
                    .unwrap_or_default()
            }
        };
        let preds_of = |b: usize| -> Vec<usize> {
            if b == func.entry.index() || preds[b].is_empty() {
                vec![root]
            } else {
                preds[b].iter().map(|p| p.index()).collect()
            }
        };

        // Postorder numbers from an iterative depth-first walk.
        const UNSEEN: usize = usize::MAX;
        let mut post_num = vec![UNSEEN; n + 1];
        let mut order = Vec::with_capacity(n + 1);
        let mut seen = vec![false; n + 1];
        let mut stack = vec![(root, succs(root), 0usize)];
        seen[root] = true;
        while let Some((b, ss, i)) = stack.last_mut() {
            if let Some(&s) = ss.get(*i) {
                *i += 1;
                if !seen[s] {
                    seen[s] = true;
                    let next = succs(s);
                    stack.push((s, next, 0));
                }
            } else {
                post_num[*b] = order.len();
                order.push(*b);
                stack.pop();
            }
        }

        let mut idom = vec![UNSEEN; n + 1];
        idom[root] = root;
        let intersect = |idom: &[usize], mut a: usize, mut b: usize| {
            while a != b {
                #[cfg(test)]
                DOM_STEPS.with(|c| c.set(c.get() + 1));
                while post_num[a] < post_num[b] {
                    a = idom[a];
                }
                while post_num[b] < post_num[a] {
                    b = idom[b];
                }
            }
            a
        };
        let mut changed = true;
        while changed {
            changed = false;
            for &b in order.iter().rev() {
                if b == root {
                    continue;
                }
                let mut new = UNSEEN;
                for p in preds_of(b) {
                    #[cfg(test)]
                    DOM_STEPS.with(|c| c.set(c.get() + 1));
                    if idom[p] == UNSEEN {
                        continue;
                    }
                    new = if new == UNSEEN { p } else { intersect(&idom, p, new) };
                }
                if idom[b] != new {
                    idom[b] = new;
                    changed = true;
                }
            }
        }

        // Preorder spans over the tree.
        let mut children = vec![Vec::new(); n + 1];
        for b in 0..n {
            if idom[b] != UNSEEN {
                children[idom[b]].push(b);
            }
        }
        let mut span = vec![(0u32, 0u32); n + 1];
        let mut clock = 0u32;
        let mut stack = vec![(root, 0usize)];
        span[root].0 = clock;
        while let Some((b, i)) = stack.last_mut() {
            if let Some(&c) = children[*b].get(*i) {
                *i += 1;
                clock += 1;
                span[c].0 = clock;
                stack.push((c, 0));
            } else {
                span[*b].1 = clock;
                stack.pop();
            }
        }
        span.truncate(n);
        Dominators {
            idom: (0..n)
                .map(|b| (idom[b] != UNSEEN && idom[b] != root).then(|| BlockId(idom[b] as u32)))
                .collect(),
            span,
            reached: (0..n).map(|b| idom[b] != UNSEEN).collect(),
        }
    }

    /// Whether every path from a root to `b` passes through `d`; a block
    /// dominates itself.
    pub(super) fn dominates(&self, d: BlockId, b: BlockId) -> bool {
        if !self.reached[b.index()] {
            return true;
        }
        if !self.reached[d.index()] {
            return false;
        }
        let (ds, de) = self.span[d.index()];
        let (bs, be) = self.span[b.index()];
        ds <= bs && be <= de
    }

    /// The closest block that dominates `b` other than `b` itself; `None`
    /// for a root and for a block no root reaches.
    pub(super) fn idom(&self, b: BlockId) -> Option<BlockId> {
        self.idom[b.index()]
    }
}

fn loop_body(preds: &[Vec<BlockId>], latch: BlockId, header: BlockId) -> HashSet<BlockId> {
    let mut body = HashSet::from([header, latch]);
    let mut work = vec![latch];
    while let Some(x) = work.pop() {
        if x == header {
            continue;
        }
        for p in &preds[x.index()] {
            if body.insert(*p) {
                work.push(*p);
            }
        }
    }
    body
}

/// What a loop may change while it runs.
struct LoopEffects {
    /// An element store, an append, or code that may write: an impure
    /// call or native, a coroutine step.
    elements: bool,
    /// A field store, or code that may write. An element store does not
    /// change a field, nor a field store an element (#1559).
    fields: bool,
    /// An append, or code that may append; with lengths not steady, any
    /// allocation too (a finalizer may resize).
    lengths: bool,
}

impl LoopEffects {
    /// Any store: elements or fields.
    fn heap(&self) -> bool {
        self.elements || self.fields
    }
}

fn loop_effects(func: &MirFunc, lp: &LoopInfo) -> LoopEffects {
    let mut fx = LoopEffects { elements: false, fields: false, lengths: false };
    let calls = func.call_facts();
    for b in &func.blocks {
        if !lp.blocks.contains(&b.id) {
            continue;
        }
        for inst in &b.insts {
            fx.elements |= inst.writes_elements(&calls);
            fx.fields |= inst.writes_fields(&calls);
            fx.lengths |= inst.may_resize(&calls);
        }
    }
    fx
}

/// How an instruction may move ahead of its loop.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Motion {
    /// Computing it early changes nothing.
    Free,
    /// It may trap: it moves only where the loop is sure to compute it.
    Traps,
}

/// The loop's `while` test, copied in front of code that may trap: the
/// header computes only cheap values and branches into the body or out.
struct Guard {
    /// The header's successor in the loop.
    body: BlockId,
    /// Whether the body runs when the condition holds.
    when: bool,
}

fn guard_shape(func: &MirFunc, lp: &LoopInfo) -> Option<Guard> {
    let header = func.block(lp.header);
    let Some(Terminator::Br { taken, not_taken, .. }) = header.term else {
        return None;
    };
    let body = match (lp.blocks.contains(&taken), lp.blocks.contains(&not_taken)) {
        (true, false) => Guard { body: taken, when: true },
        (false, true) => Guard { body: not_taken, when: false },
        _ => return None,
    };
    let cheap = header.insts.iter().filter(|i| !i.is_phi()).all(|i| cheap_test(func, i));
    (cheap && header.insts.len() <= 32).then_some(body)
}

/// An instruction worth computing twice that cannot trap.
fn cheap_test(func: &MirFunc, inst: &MirInst) -> bool {
    match inst {
        MirInst::Const { .. } | MirInst::Cmp { .. } | MirInst::Cast { .. } | MirInst::ArrayLen { .. } => true,
        MirInst::Bin { .. } | MirInst::Unary { .. } => !int_may_trap(func, inst) && !inst.may_trap(func),
        _ => false,
    }
}

/// Blocks every iteration that enters the body runs before it can leave
/// the loop or reach the header again.
fn every_iteration(func: &MirFunc, lp: &LoopInfo, body: BlockId) -> HashSet<BlockId> {
    let mut every = HashSet::from([lp.header]);
    for &x in &lp.blocks {
        if x == lp.header {
            continue;
        }
        // From the body's first block without passing `x`: may an
        // iteration end?
        let mut seen = HashSet::new();
        let mut work = vec![body];
        let mut escapes = false;
        while let Some(b) = work.pop() {
            if b == x || !seen.insert(b) {
                continue;
            }
            let succs = match &func.block(b).term {
                Some(t) => t.succs(),
                None => Vec::new(),
            };
            if succs.is_empty() || succs.iter().any(|s| *s == lp.header || !lp.blocks.contains(s)) {
                escapes = true;
                break;
            }
            work.extend(succs);
        }
        if !escapes {
            every.insert(x);
        }
    }
    every
}

fn hoist_loop(func: &mut MirFunc, lp: &LoopInfo, dense: bool) -> usize {
    let fx = loop_effects(func, lp);
    let guard = guard_shape(func, lp);
    let every = guard.as_ref().map(|g| every_iteration(func, lp, g.body)).unwrap_or_default();
    let consts: std::collections::HashMap<ValueId, i64> = func
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter_map(|i| match i {
            MirInst::Const { dest, c: super::inst::MirConst::I64(k) } => Some((*dest, *k)),
            _ => None,
        })
        .collect();
    let defined = values_defined_in(func, &lp.blocks);
    let mut invariant: HashSet<ValueId> = HashSet::new();
    for i in 0..func.types.len() {
        let v = ValueId(i as u32);
        if !defined.contains(&v) {
            invariant.insert(v);
        }
    }
    // LIR keeps a literal in the loop (an immediate operand there); code
    // that moves reads a copy of it.
    let literals: std::collections::HashMap<ValueId, MirInst> = if dense {
        std::collections::HashMap::new()
    } else {
        func.blocks
            .iter()
            .filter(|b| lp.blocks.contains(&b.id))
            .flat_map(|b| &b.insts)
            .filter(|i| matches!(i, MirInst::Const { .. }))
            .map(|i| (i.dest(), i.clone()))
            .collect()
    };
    invariant.extend(literals.keys().copied());
    // A compare or unary op on what the loop reads as is stays when only
    // branches test it: it fuses with them.
    let reads = invariant.clone();
    let mut tested: HashSet<ValueId> = HashSet::new();
    let mut read_elsewhere: HashSet<ValueId> = HashSet::new();
    for b in &func.blocks {
        b.insts.iter().for_each(|i| read_elsewhere.extend(i.operands()));
        match &b.term {
            Some(Terminator::Br { cond, .. }) => {
                tested.insert(*cond);
            }
            Some(Terminator::JumpIfMatch { scrutinee, payloads, .. }) => {
                read_elsewhere.insert(*scrutinee);
                read_elsewhere.extend(payloads);
            }
            Some(Terminator::Return { lo, hi }) => read_elsewhere.extend(lo.iter().chain(hi)),
            Some(Terminator::Jump { .. } | Terminator::Unreachable) | None => {}
        }
    }

    // Values that move behind the guard, and everything that reads one.
    let mut guarded: HashSet<ValueId> = HashSet::new();
    let mut todo: Vec<ValueId> = Vec::new();
    loop {
        let mut found = None;
        for b in &func.blocks {
            if !lp.blocks.contains(&b.id) {
                continue;
            }
            for (idx, inst) in b.insts.iter().enumerate() {
                if invariant.contains(&inst.dest()) {
                    continue;
                }
                let Some(motion) = motion(func, inst, &fx, &consts, dense) else {
                    continue;
                };
                let ops = inst.operands();
                if !ops.iter().all(|o| invariant.contains(o)) {
                    continue;
                }
                if matches!(inst, MirInst::Cmp { .. } | MirInst::Unary { .. })
                    && ops.iter().all(|o| reads.contains(o))
                    && tested.contains(&inst.dest())
                    && !read_elsewhere.contains(&inst.dest())
                {
                    continue;
                }
                let behind = ops.iter().any(|o| guarded.contains(o));
                let numeric = func.ty(inst.dest()).is_numeric();
                let place = match motion {
                    // In the header, ahead of anything observable: the loop
                    // computes it whenever it is entered.
                    Motion::Traps if !behind && b.id == lp.header && nothing_observable_before(func, &b.insts[..idx]) => Some(false),
                    Motion::Traps if numeric && guard.is_some() && every.contains(&b.id) => Some(true),
                    Motion::Traps => None,
                    Motion::Free if behind => numeric.then_some(true),
                    Motion::Free => Some(false),
                };
                if let Some(g) = place {
                    found = Some((inst.dest(), g));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let Some((dest, g)) = found else {
            break;
        };
        invariant.insert(dest);
        if g {
            guarded.insert(dest);
        }
        todo.push(dest);
    }
    if todo.is_empty() {
        return 0;
    }

    let old_len = func.blocks.len();
    let pre = ensure_preheader(func, lp);
    let mut free = Vec::new();
    let mut behind = Vec::new();
    for dest in todo {
        for block in &mut func.blocks {
            if let Some(idx) = block.insts.iter().position(|i| i.dest() == dest) {
                let inst = block.insts.remove(idx);
                if guarded.contains(&dest) {
                    behind.push(inst);
                } else {
                    free.push(inst);
                }
                break;
            }
        }
    }
    let n = free.len() + behind.len();
    let mut copies: std::collections::HashMap<ValueId, ValueId> = std::collections::HashMap::new();
    let mut lits = Vec::new();
    for inst in free.iter_mut().chain(behind.iter_mut()) {
        inst.rewrite_values(|v| {
            let Some(lit) = literals.get(&v) else {
                return v;
            };
            *copies.entry(v).or_insert_with(|| {
                let fresh = ValueId(func.types.len() as u32);
                func.types.push(func.types[v.index()]);
                let mut lit = lit.clone();
                set_test_dest(&mut lit, fresh);
                lits.push(lit);
                fresh
            })
        });
    }
    // At the end: the preheader may itself define the operands.
    func.block_mut(pre).insts.extend(lits.into_iter().chain(free));
    if let (false, Some(guard)) = (behind.is_empty(), guard) {
        guard_hoisted(func, lp.header, pre, &guard, behind);
    }
    // New blocks go after the preheader (a new one just ahead of the
    // header), as the source order would have them: emission reads a back
    // edge as one to an earlier block.
    if func.blocks.len() > old_len {
        let new: Vec<BlockId> = (old_len..func.blocks.len()).map(|i| BlockId(i as u32)).collect();
        let at = if pre.index() >= old_len { lp.header } else { pre };
        let mut layout = Vec::with_capacity(func.blocks.len());
        for b in (0..old_len).map(|b| BlockId(b as u32)) {
            if b == at && at == lp.header {
                layout.extend(&new);
            }
            layout.push(b);
            if b == at && at != lp.header {
                layout.extend(&new);
            }
        }
        func.relayout(&layout);
    }
    n
}

/// `pre` (the preheader, ending in a jump to `header`) tests the loop's
/// condition once; when the body would run, a new block computes
/// `behind`; a join block then enters the loop, where each moved value is
/// read through a φ (a dummy zero on the path where the loop runs no
/// iteration and never reads it).
fn guard_hoisted(func: &mut MirFunc, header: BlockId, pre: BlockId, guard: &Guard, behind: Vec<MirInst>) {
    let then = BlockId(func.blocks.len() as u32);
    let join = BlockId(then.0 + 1);
    // The header's test on the values the loop is entered with.
    let mut map: std::collections::HashMap<ValueId, ValueId> = std::collections::HashMap::new();
    let mut copies = Vec::new();
    for inst in func.block(header).insts.clone() {
        match inst {
            MirInst::Phi { dest, args, .. } => {
                if let Some(&(_, v)) = args.iter().find(|(b, _)| *b == pre) {
                    map.insert(dest, v);
                }
            }
            mut copy => {
                let old = copy.dest();
                copy.rewrite_values(|v| map.get(&v).copied().unwrap_or(v));
                let fresh = ValueId(func.types.len() as u32);
                func.types.push(func.ty(old));
                map.insert(old, fresh);
                set_test_dest(&mut copy, fresh);
                copies.push(copy);
            }
        }
    }
    let Some(Terminator::Br { cond, .. }) = func.block(header).term else {
        unreachable!("guard_shape checked the header");
    };
    let test = map.get(&cond).copied().unwrap_or(cond);
    // A dummy per type, and a φ per moved value read outside the moved code.
    let mut read: HashSet<ValueId> = HashSet::new();
    for block in &func.blocks {
        for inst in &block.insts {
            read.extend(inst.operands());
        }
        if let Some(term) = &block.term {
            let mut term = term.clone();
            term.rewrite_values(|v| {
                read.insert(v);
                v
            });
        }
    }
    read.extend(func.debug_slots.values().copied());
    read.extend(func.slot_env.values().flatten().map(|&(_, v)| v));
    let mut zeros: Vec<(super::ty::MirTy, ValueId)> = Vec::new();
    let mut phis = Vec::new();
    let mut rename: std::collections::HashMap<ValueId, ValueId> = std::collections::HashMap::new();
    for inst in &behind {
        let v = inst.dest();
        if !read.contains(&v) {
            continue;
        }
        let ty = func.ty(v);
        let zero = match zeros.iter().find(|(t, _)| *t == ty) {
            Some(&(_, z)) => z,
            None => {
                let z = ValueId(func.types.len() as u32);
                func.types.push(ty);
                let c = match ty {
                    super::ty::MirTy::F64 => super::inst::MirConst::F64(0),
                    super::ty::MirTy::Bool => super::inst::MirConst::Bool(false),
                    _ => super::inst::MirConst::I64(0),
                };
                copies.push(MirInst::Const { dest: z, c });
                zeros.push((ty, z));
                z
            }
        };
        let p = ValueId(func.types.len() as u32);
        func.types.push(ty);
        phis.push(MirInst::Phi { dest: p, ty, args: vec![(then, v), (pre, zero)] });
        rename.insert(v, p);
    }
    let pre_block = func.block_mut(pre);
    pre_block.insts.extend(copies);
    pre_block.term = Some(if guard.when {
        Terminator::Br { cond: test, taken: then, not_taken: join }
    } else {
        Terminator::Br { cond: test, taken: join, not_taken: then }
    });
    let mut then_block = super::func::MirBlock::new(then);
    then_block.insts = behind;
    then_block.term = Some(Terminator::Jump { dest: join });
    let mut join_block = super::func::MirBlock::new(join);
    join_block.insts = phis;
    join_block.term = Some(Terminator::Jump { dest: header });
    func.blocks.push(then_block);
    func.blocks.push(join_block);
    // The header is entered from the join now; every other read of a
    // moved value reads its φ.
    for block in &mut func.blocks {
        if block.id == then || block.id == join {
            continue;
        }
        for inst in &mut block.insts {
            if let MirInst::Phi { args, .. } = inst
                && block.id == header
            {
                for (b, _) in args.iter_mut() {
                    if *b == pre {
                        *b = join;
                    }
                }
            }
            inst.rewrite_values(|v| rename.get(&v).copied().unwrap_or(v));
        }
        if let Some(term) = &mut block.term {
            term.rewrite_values(|v| rename.get(&v).copied().unwrap_or(v));
        }
    }
    func.rewrite_debug_slots(|v| rename.get(&v).copied().unwrap_or(v));
    for env in func.slot_env.values_mut() {
        for (_, v) in env.iter_mut() {
            *v = rename.get(v).copied().unwrap_or(*v);
        }
    }
}

/// Rename the value a [`cheap_test`] instruction defines.
fn set_test_dest(inst: &mut MirInst, fresh: ValueId) {
    match inst {
        MirInst::Const { dest, .. }
        | MirInst::Cmp { dest, .. }
        | MirInst::Cast { dest, .. }
        | MirInst::ArrayLen { dest, .. }
        | MirInst::Bin { dest, .. }
        | MirInst::Unary { dest, .. } => *dest = fresh,
        _ => unreachable!("not a cheap test"),
    }
}

/// How `inst` may move out of a loop with effects `fx`, or `None` when it
/// stays. `dense`: as [`licm`] takes it.
fn motion(
    func: &MirFunc,
    inst: &MirInst,
    fx: &LoopEffects,
    consts: &std::collections::HashMap<ValueId, i64>,
    dense: bool,
) -> Option<Motion> {
    if !dense && (func.ty(inst.dest()).is_heap_word() || matches!(inst, MirInst::Const { .. })) {
        return None;
    }
    let free = |ok: bool| ok.then_some(Motion::Free);
    match inst {
        MirInst::Phi { .. } => None,
        // A division by a literal other than 0 and -1 cannot trap.
        MirInst::Bin { op: MirBinOp::Div | MirBinOp::Rem, ty, rhs, .. } if ty.is_int() => {
            Some(match consts.get(rhs) {
                Some(&k) if k != 0 && k != -1 => Motion::Free,
                _ => Motion::Traps,
            })
        }
        MirInst::Bin { op: MirBinOp::Pow, ty, .. } if ty.is_int() => Some(Motion::Traps),
        // On literals that do not overflow: nothing to trap.
        &MirInst::Bin { op, ty, lhs, rhs, .. }
            if ty.is_int()
                && let (Some(&a), Some(&b)) = (consts.get(&lhs), consts.get(&rhs))
                && match op {
                    MirBinOp::Add => a.checked_add(b).is_some(),
                    MirBinOp::Sub => a.checked_sub(b).is_some(),
                    MirBinOp::Mul => a.checked_mul(b).is_some(),
                    MirBinOp::Shl | MirBinOp::Shr => (0..64).contains(&b),
                    _ => false,
                } =>
        {
            Some(Motion::Free)
        }
        MirInst::Bin { .. } | MirInst::Unary { .. } if int_may_trap(func, inst) => Some(Motion::Traps),
        MirInst::Const { .. } | MirInst::Bin { .. } | MirInst::Cmp { .. } | MirInst::Unary { .. } | MirInst::Cast { .. } => {
            Some(Motion::Free)
        }
        MirInst::HostInvoke { native_id, .. } => free(super::effects::host_may_hoist(*native_id)),
        // A checked read traps out of bounds; an unchecked one was proven
        // in bounds where it is.
        // An element or the length of an array nothing writes, and the
        // slot of a module `const` table (#1561).
        MirInst::Index { dest, .. } if func.invariant_loads.contains(dest) => Some(Motion::Traps),
        MirInst::ArrayLen { dest, .. } | MirInst::LoadStatic { dest, .. } if func.invariant_loads.contains(dest) => {
            Some(Motion::Free)
        }
        MirInst::Index { .. } => (!fx.elements && !fx.lengths).then_some(Motion::Traps),
        MirInst::ArrayLen { .. } => free(!fx.lengths),
        // A readonly class's field never changes (#1560).
        MirInst::HeapFieldLoad { dest, .. } if func.invariant_loads.contains(dest) => Some(Motion::Free),
        MirInst::HeapFieldLoad { .. } => free(!fx.fields),
        MirInst::Call { dest, args, .. } if func.pure_calls.contains(dest) => {
            let reads_heap = args.iter().any(|a| super::cse::reads_heap(func.ty(*a)));
            (!reads_heap || !fx.heap() && !fx.lengths).then_some(Motion::Traps)
        }
        MirInst::Alloc {
            kind: super::inst::MirAllocKind::Array | super::inst::MirAllocKind::Tuple,
            ..
        }
        | MirInst::GcBarrier { .. } => free(dense && !fx.heap()),
        MirInst::String { .. } | MirInst::CodePtr { .. } => free(dense),
        _ => None,
    }
}

/// An int `+ - *`, shift or negation, which traps on overflow or a bad
/// shift count.
pub(crate) fn int_may_trap(func: &MirFunc, inst: &MirInst) -> bool {
    match inst {
        // A shift traps on a count outside 0..=63 (#1076).
        MirInst::Bin { op: MirBinOp::Add | MirBinOp::Sub | MirBinOp::Mul | MirBinOp::Shl | MirBinOp::Shr, ty, .. } => ty.is_int(),
        MirInst::Unary { op: super::inst::MirUnaryOp::Neg, dest, .. } => func.ty(*dest).is_int(),
        _ => false,
    }
}

fn nothing_observable_before(func: &MirFunc, before: &[MirInst]) -> bool {
    before.iter().all(|i| {
        i.is_phi()
            || matches!(i, MirInst::Const { .. } | MirInst::Cmp { .. } | MirInst::Cast { .. })
            || matches!(i, MirInst::Bin { ty, .. } if ty.is_float())
            || matches!(i, MirInst::Unary { .. }) && !int_may_trap(func, i)
    })
}

fn values_defined_in(func: &MirFunc, blocks: &HashSet<BlockId>) -> HashSet<ValueId> {
    let mut out = HashSet::new();
    for b in &func.blocks {
        if !blocks.contains(&b.id) {
            continue;
        }
        for inst in &b.insts {
            out.extend(inst.dests());
        }
    }
    out
}

pub(super) fn ensure_preheader(func: &mut MirFunc, lp: &LoopInfo) -> BlockId {
    let preds = func.preds();
    let header = lp.header;
    let external: Vec<BlockId> = preds[header.index()]
        .iter()
        .copied()
        .filter(|p| !lp.blocks.contains(p))
        .collect();
    if external.len() == 1 {
        let p = external[0];
        if matches!(
            func.block(p).term,
            Some(Terminator::Jump { dest }) if dest == header
        ) {
            return p;
        }
    }
    if external.is_empty() && func.entry == header {
        return insert_entry_preheader(func, header);
    }
    insert_join_preheader(func, header, &external)
}

fn insert_entry_preheader(func: &mut MirFunc, header: BlockId) -> BlockId {
    let ph = BlockId(func.blocks.len() as u32);
    let mut block = super::func::MirBlock::new(ph);
    block.term = Some(Terminator::Jump { dest: header });
    func.blocks.push(block);
    func.entry = ph;
    split_header_phis(func, header, ph, &[]);
    ph
}

fn insert_join_preheader(func: &mut MirFunc, header: BlockId, external: &[BlockId]) -> BlockId {
    let ph = BlockId(func.blocks.len() as u32);
    let mut block = super::func::MirBlock::new(ph);
    block.term = Some(Terminator::Jump { dest: header });
    func.blocks.push(block);
    for pred in external {
        if let Some(term) = &mut func.block_mut(*pred).term {
            retarget(term, header, ph);
        }
    }
    split_header_phis(func, header, ph, external);
    ph
}

fn retarget(term: &mut Terminator, from: BlockId, to: BlockId) {
    match term {
        Terminator::Jump { dest } if *dest == from => *dest = to,
        Terminator::Br {
            taken, not_taken, ..
        }
        | Terminator::JumpIfMatch {
            taken, not_taken, ..
        } => {
            if *taken == from {
                *taken = to;
            }
            if *not_taken == from {
                *not_taken = to;
            }
        }
        _ => {}
    }
}

fn split_header_phis(func: &mut MirFunc, header: BlockId, ph: BlockId, external: &[BlockId]) {
    let ext: HashSet<BlockId> = external.iter().copied().collect();
    let header_insts = func.block(header).insts.clone();
    let mut ph_phis = Vec::new();
    let mut new_header = Vec::new();
    for inst in header_insts {
        let MirInst::Phi { dest, ty, args } = inst else {
            new_header.push(inst);
            continue;
        };
        let (ext_args, int_args): (Vec<_>, Vec<_>) =
            args.into_iter().partition(|(b, _)| ext.contains(b));
        if ext_args.is_empty() {
            new_header.push(MirInst::Phi {
                dest,
                ty,
                args: int_args,
            });
            continue;
        }
        let first = ext_args[0].1;
        let same = ext_args.iter().all(|(_, v)| *v == first);
        let incoming = if same && ext_args.len() == 1 || same {
            first
        } else {
            let pdest = ValueId(func.types.len() as u32);
            func.types.push(ty);
            ph_phis.push(MirInst::Phi {
                dest: pdest,
                ty,
                args: ext_args,
            });
            pdest
        };
        let mut args = int_args;
        args.push((ph, incoming));
        new_header.push(MirInst::Phi { dest, ty, args });
    }
    {
        let block = func.block_mut(ph);
        let mut insts = ph_phis;
        insts.append(&mut block.insts);
        block.insts = insts;
    }
    let rest: Vec<MirInst> = func
        .block(header)
        .insts
        .iter()
        .filter(|i| !i.is_phi())
        .cloned()
        .collect();
    new_header.extend(rest);
    func.block_mut(header).insts = new_header;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::builder::MirBuilder;
    use crate::mir::{MirBinOp, MirCmpOp, MirConst, MirTy};

    fn count_bin_in(func: &MirFunc, blocks: &HashSet<BlockId>, op: MirBinOp) -> usize {
        func.blocks
            .iter()
            .filter(|b| blocks.contains(&b.id))
            .flat_map(|b| b.insts.iter())
            .filter(|i| matches!(i, MirInst::Bin { op: o, .. } if *o == op))
            .count()
    }

    fn count_const_in(func: &MirFunc, blocks: &HashSet<BlockId>) -> usize {
        func.blocks
            .iter()
            .filter(|b| blocks.contains(&b.id))
            .flat_map(|b| b.insts.iter())
            .filter(|i| matches!(i, MirInst::Const { .. }))
            .count()
    }

    fn loop_blocks(func: &MirFunc) -> HashSet<BlockId> {
        natural_loops(func)
            .into_iter()
            .min_by_key(|lp| lp.blocks.len())
            .map(|lp| lp.blocks)
            .unwrap_or_default()
    }

    #[test]
    fn hoists_invariant_fdiv_and_const() {
        let mut b = MirBuilder::new("licm");
        let scale = b.add_param(MirTy::F64).unwrap();
        let n = b.add_param(MirTy::I64).unwrap();
        const I: crate::mir::inst::LocalId = crate::mir::inst::LocalId(0);
        const S: crate::mir::inst::LocalId = crate::mir::inst::LocalId(1);
        let i0 = b.ins_const(MirConst::I64(0)).unwrap();
        let s0 = b.ins_const(MirConst::f64(0.0)).unwrap();
        b.def_local(I, i0).unwrap();
        b.def_local(S, s0).unwrap();
        let header = b.create_block();
        let body = b.create_block();
        let exit = b.create_block();
        b.jump(header).unwrap();

        b.switch_to_block(header);
        let i = b.use_local(I, MirTy::I64).unwrap();
        let cond = b.ins_cmp(MirCmpOp::Lt, i, n).unwrap();
        b.branch(cond, body, exit).unwrap();

        b.switch_to_block(body);
        let one_f = b.ins_const(MirConst::f64(1.0)).unwrap();
        let inv = b.ins_binop(MirBinOp::Div, one_f, scale).unwrap();
        let xf = b
            .ins_cast(crate::mir::MirCastKind::IntToFloat, MirTy::F64, i)
            .unwrap();
        let term = b.ins_binop(MirBinOp::Mul, inv, xf).unwrap();
        let s = b.use_local(S, MirTy::F64).unwrap();
        let s1 = b.ins_binop(MirBinOp::Add, s, term).unwrap();
        let one = b.ins_const(MirConst::I64(1)).unwrap();
        let i1 = b.ins_binop(MirBinOp::Add, i, one).unwrap();
        b.def_local(S, s1).unwrap();
        b.def_local(I, i1).unwrap();
        b.jump(header).unwrap();

        b.switch_to_block(exit);
        let s_out = b.use_local(S, MirTy::F64).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(s_out)).unwrap();

        let mut f = b.finish().unwrap();
        f.verify().unwrap();
        let before = loop_blocks(&f);
        assert_eq!(count_bin_in(&f, &before, MirBinOp::Div), 1);
        assert!(count_const_in(&f, &before) >= 1);

        assert!(licm(&mut f, true) >= 1);
        f.verify().unwrap();
        let after = loop_blocks(&f);
        assert_eq!(
            count_bin_in(&f, &after, MirBinOp::Div),
            0,
            "invariant fdiv must leave the loop"
        );
        assert_eq!(
            count_bin_in(&f, &after, MirBinOp::Mul),
            1,
            "variant mul stays"
        );
    }

    #[test]
    fn does_not_hoist_variant_mul() {
        let mut b = MirBuilder::new("var");
        let n = b.add_param(MirTy::I64).unwrap();
        const I: crate::mir::inst::LocalId = crate::mir::inst::LocalId(0);
        let i0 = b.ins_const(MirConst::I64(0)).unwrap();
        b.def_local(I, i0).unwrap();
        let header = b.create_block();
        let body = b.create_block();
        let exit = b.create_block();
        b.jump(header).unwrap();
        b.switch_to_block(header);
        let i = b.use_local(I, MirTy::I64).unwrap();
        let cond = b.ins_cmp(MirCmpOp::Lt, i, n).unwrap();
        b.branch(cond, body, exit).unwrap();
        b.switch_to_block(body);
        let two = b.ins_const(MirConst::I64(2)).unwrap();
        let m = b.ins_binop(MirBinOp::Mul, i, two).unwrap();
        let one = b.ins_const(MirConst::I64(1)).unwrap();
        let i1 = b.ins_binop(MirBinOp::Add, m, one).unwrap();
        b.def_local(I, i1).unwrap();
        b.jump(header).unwrap();
        b.switch_to_block(exit);
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(i)).unwrap();
        let mut f = b.finish().unwrap();
        licm(&mut f, true);
        f.verify().unwrap();
        let lp = loop_blocks(&f);
        assert_eq!(count_bin_in(&f, &lp, MirBinOp::Mul), 1);
    }

    /// A two-slot call's tag word is defined in the loop; a test on it must
    /// not be hoisted (it read a stale tag and looped forever).
    /// An invariant int `**` may trap (overflow, a negative exponent), so
    /// it stays where the loop body reaches it, as `/` does.
    #[test]
    fn int_pow_moves_only_behind_the_loop_test() {
        let mut b = MirBuilder::new("ipow");
        let x = b.add_param(MirTy::I64).unwrap();
        let n = b.add_param(MirTy::I64).unwrap();
        const I: crate::mir::inst::LocalId = crate::mir::inst::LocalId(0);
        let i0 = b.ins_const(MirConst::I64(0)).unwrap();
        b.def_local(I, i0).unwrap();
        let header = b.create_block();
        let body = b.create_block();
        let exit = b.create_block();
        b.jump(header).unwrap();
        b.switch_to_block(header);
        let i = b.use_local(I, MirTy::I64).unwrap();
        let cond = b.ins_cmp(MirCmpOp::Lt, i, n).unwrap();
        b.branch(cond, body, exit).unwrap();
        b.switch_to_block(body);
        let p = b.ins_binop(MirBinOp::Pow, x, n).unwrap();
        let i1 = b.ins_binop(MirBinOp::Add, i, p).unwrap();
        b.def_local(I, i1).unwrap();
        b.jump(header).unwrap();
        b.switch_to_block(exit);
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(i)).unwrap();
        let mut f = b.finish().unwrap();
        licm(&mut f, true);
        f.verify().unwrap();
        let lp = loop_blocks(&f);
        assert_eq!(count_bin_in(&f, &lp, MirBinOp::Pow), 0);
        // A negative exponent traps: not where a loop that runs no
        // iteration would compute it.
        let at = def_block(&f, |i| matches!(i, MirInst::Bin { op: MirBinOp::Pow, .. }));
        assert_ne!(at, f.entry);
        assert!(f.preds_of(at).iter().all(|&p| matches!(f.block(p).term, Some(Terminator::Br { .. }))));
    }

    #[test]
    fn does_not_hoist_use_of_two_slot_call_hi_word() {
        let mut b = MirBuilder::new("hi");
        b.allow_effects = true;
        let n = b.add_param(MirTy::I64).unwrap();
        let header = b.create_block();
        let exit = b.create_block();
        b.jump(header).unwrap();
        b.switch_to_block(header);
        let abi = crate::layout::DenseAbi {
            params: vec![MirTy::I64],
            ret: MirTy::I64,
            ret_hi: Some(MirTy::I64),
        };
        let (_, tag) = b.ins_call(crate::il::Label(9), vec![n], &abi).unwrap();
        let zero = b.ins_const(MirConst::I64(0)).unwrap();
        let some = b.ins_cmp(MirCmpOp::Ne, tag.unwrap(), zero).unwrap();
        b.branch(some, header, exit).unwrap();
        b.switch_to_block(exit);
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(n)).unwrap();
        let mut f = b.finish().unwrap();
        licm(&mut f, true);
        f.verify().unwrap();
        let header_cmps = f
            .block(header)
            .insts
            .iter()
            .filter(|i| matches!(i, MirInst::Cmp { .. }))
            .count();
        assert_eq!(header_cmps, 1, "tag test must stay in the loop");
    }

    fn count_host_in(func: &MirFunc, blocks: &HashSet<BlockId>, id: u16) -> usize {
        func.blocks
            .iter()
            .filter(|b| blocks.contains(&b.id))
            .flat_map(|b| b.insts.iter())
            .filter(|i| matches!(i, MirInst::HostInvoke { native_id, .. } if *native_id == id))
            .count()
    }

    #[test]
    fn hoists_invariant_math_host() {
        let mut b = MirBuilder::new("math");
        let n = b.add_param(MirTy::I64).unwrap();
        const I: crate::mir::inst::LocalId = crate::mir::inst::LocalId(0);
        const S: crate::mir::inst::LocalId = crate::mir::inst::LocalId(1);
        let i0 = b.ins_const(MirConst::I64(0)).unwrap();
        let s0 = b.ins_const(MirConst::f64(0.0)).unwrap();
        b.def_local(I, i0).unwrap();
        b.def_local(S, s0).unwrap();
        let header = b.create_block();
        let body = b.create_block();
        let exit = b.create_block();
        b.jump(header).unwrap();
        b.switch_to_block(header);
        let i = b.use_local(I, MirTy::I64).unwrap();
        let cond = b.ins_cmp(MirCmpOp::Lt, i, n).unwrap();
        b.branch(cond, body, exit).unwrap();
        b.switch_to_block(body);
        let x = b.ins_const(MirConst::f64(1.0)).unwrap();
        let s_host = b.ins_host_invoke(common::MATH_SIN_ID, vec![x]).unwrap();
        let s = b.use_local(S, MirTy::F64).unwrap();
        let s1 = b.ins_binop(MirBinOp::Add, s, s_host).unwrap();
        let one = b.ins_const(MirConst::I64(1)).unwrap();
        let i1 = b.ins_binop(MirBinOp::Add, i, one).unwrap();
        b.def_local(S, s1).unwrap();
        b.def_local(I, i1).unwrap();
        b.jump(header).unwrap();
        b.switch_to_block(exit);
        let s_out = b.use_local(S, MirTy::F64).unwrap();
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(s_out)).unwrap();
        let mut f = b.finish().unwrap();
        assert!(licm(&mut f, true) >= 1);
        f.verify().unwrap();
        let after = loop_blocks(&f);
        assert_eq!(
            count_host_in(&f, &after, common::MATH_SIN_ID),
            0,
            "pure math HostInvoke may hoist"
        );
    }

    #[test]
    fn does_not_hoist_impure_clock_host() {
        let mut b = MirBuilder::new("clk");
        b.allow_effects = true;
        let n = b.add_param(MirTy::I64).unwrap();
        const I: crate::mir::inst::LocalId = crate::mir::inst::LocalId(0);
        const S: crate::mir::inst::LocalId = crate::mir::inst::LocalId(1);
        let i0 = b.ins_const(MirConst::I64(0)).unwrap();
        let s0 = b.ins_const(MirConst::I64(0)).unwrap();
        b.def_local(I, i0).unwrap();
        b.def_local(S, s0).unwrap();
        let header = b.create_block();
        let body = b.create_block();
        let exit = b.create_block();
        b.jump(header).unwrap();
        b.switch_to_block(header);
        let i = b.use_local(I, MirTy::I64).unwrap();
        let cond = b.ins_cmp(MirCmpOp::Lt, i, n).unwrap();
        b.branch(cond, body, exit).unwrap();
        b.switch_to_block(body);
        let t = b
            .ins_host_invoke(common::CLOCK_MONO_NANOS_ID, vec![])
            .unwrap();
        let s = b.use_local(S, MirTy::I64).unwrap();
        let s1 = b.ins_binop(MirBinOp::Add, s, t).unwrap();
        let one = b.ins_const(MirConst::I64(1)).unwrap();
        let i1 = b.ins_binop(MirBinOp::Add, i, one).unwrap();
        b.def_local(S, s1).unwrap();
        b.def_local(I, i1).unwrap();
        b.jump(header).unwrap();
        b.switch_to_block(exit);
        let s_out = b.use_local(S, MirTy::I64).unwrap();
        b.set_ret_ty(MirTy::I64);
        b.ret(Some(s_out)).unwrap();
        let mut f = b.finish().unwrap();
        assert!(f.has_impure_host());
        assert!(f
            .blocks
            .iter()
            .flat_map(|bl| bl.insts.iter())
            .any(MirInst::is_effect_barrier));
        licm(&mut f, true);
        f.verify().unwrap();
        let after = loop_blocks(&f);
        assert_eq!(
            count_host_in(&f, &after, common::CLOCK_MONO_NANOS_ID),
            1,
            "impure clock HostInvoke must stay in the loop"
        );
    }

    /// A counted loop `while v4 < v0 { s += <body>; v4 += 1 }` with
    /// `body` spliced in (it defines `v6`, read by the sum).
    fn counted(params: &str, body: &str) -> MirFunc {
        let src = format!(
            r#"
func @f({params}) -> i64 {{
bb0:
    v2 = iconst.i64 0
    v3 = iconst.i64 1
    jump bb1
bb1:
    v4 = phi.i64 [bb0: v2, bb2: v8]
    v5 = phi.i64 [bb0: v2, bb2: v7]
    v9 = icmp.slt v4, v0
    brif v9, bb2, bb3
bb2:
{body}
    v7 = iadd v5, v6
    v8 = iadd v4, v3
    jump bb1
bb3:
    return v5
}}
"#
        );
        let f = crate::mir::parse_func(&src).expect(&src);
        f.verify().unwrap();
        f
    }

    fn in_loop(func: &MirFunc, pred: impl Fn(&MirInst) -> bool) -> usize {
        let lp = loop_blocks(func);
        func.blocks.iter().filter(|b| lp.contains(&b.id)).flat_map(|b| &b.insts).filter(|i| pred(i)).count()
    }

    /// The block holding the value `v`'s definition.
    fn def_block(func: &MirFunc, pred: impl Fn(&MirInst) -> bool) -> BlockId {
        func.blocks.iter().find(|b| b.insts.iter().any(&pred)).map(|b| b.id).expect("defined")
    }

    #[test]
    fn pure_call_moves_behind_a_copy_of_the_loop_test() {
        let mut f = counted("v0: i64, v1: i64", "    v6 = call.i64 7 v1");
        f.pure_calls.insert(ValueId(6));
        assert!(licm(&mut f, false) >= 1);
        f.verify().unwrap();
        let is_call = |i: &MirInst| matches!(i, MirInst::Call { .. });
        assert_eq!(in_loop(&f, is_call), 0, "the pure call leaves the loop");
        // A loop that runs no iteration never calls it: its block is the
        // taken arm of the copied `v4 < v0` on the entry values.
        let at = def_block(&f, is_call);
        let preds = f.preds_of(at);
        let [pre] = preds.as_slice() else { panic!("one way in: {f}") };
        let Some(Terminator::Br { cond, taken, .. }) = f.block(*pre).term else { panic!("guarded: {f}") };
        assert_eq!(taken, at);
        assert!(f.block(*pre).insts.iter().any(|i| matches!(i, MirInst::Cmp { dest, lhs, rhs, .. } if *dest == cond && *lhs == ValueId(2) && *rhs == ValueId(0))));
    }

    #[test]
    fn impure_call_stays() {
        let mut f = counted("v0: i64, v1: i64", "    v6 = call.i64 7 v1");
        licm(&mut f, false);
        f.verify().unwrap();
        assert_eq!(in_loop(&f, |i| matches!(i, MirInst::Call { .. })), 1);
    }

    #[test]
    fn trapping_int_chain_moves_behind_the_loop_test() {
        let mut f = counted("v0: i64, v1: i64", "    v10 = iconst.i64 3\n    v11 = imul v1, v10\n    v6 = iadd v11, v3");
        assert!(licm(&mut f, true) >= 1);
        f.verify().unwrap();
        assert_eq!(in_loop(&f, |i| matches!(i, MirInst::Bin { op: MirBinOp::Mul, .. })), 0);
        // The sum's own add and the step stay.
        assert_eq!(in_loop(&f, |i| matches!(i, MirInst::Bin { op: MirBinOp::Add, .. })), 2);
    }

    #[test]
    fn trapping_op_on_a_conditional_path_stays() {
        let src = r#"
func @f(v0: i64, v1: i64, v10: bool) -> i64 {
bb0:
    v2 = iconst.i64 0
    v3 = iconst.i64 1
    jump bb1
bb1:
    v4 = phi.i64 [bb0: v2, bb4: v8]
    v5 = phi.i64 [bb0: v2, bb4: v7]
    v9 = icmp.slt v4, v0
    brif v9, bb2, bb5
bb2:
    brif v10, bb3, bb4
bb3:
    v6 = imul v1, v1
    jump bb4
bb4:
    v11 = phi.i64 [bb2: v2, bb3: v6]
    v7 = iadd v5, v11
    v8 = iadd v4, v3
    jump bb1
bb5:
    return v5
}
"#;
        let mut f = crate::mir::parse_func(src).expect(src);
        licm(&mut f, true);
        f.verify().unwrap();
        assert_eq!(in_loop(&f, |i| matches!(i, MirInst::Bin { op: MirBinOp::Mul, .. })), 1);
    }

    #[test]
    fn checked_index_moves_only_behind_the_loop_test() {
        let mut f = counted("v0: i64, v1: heapref", "    v6 = index v1, v3");
        assert!(licm(&mut f, true) >= 1);
        f.verify().unwrap();
        let is_index = |i: &MirInst| matches!(i, MirInst::Index { .. });
        assert_eq!(in_loop(&f, is_index), 0);
        let at = def_block(&f, is_index);
        assert_ne!(at, f.entry, "not ahead of a loop that may run no iteration");
    }

    #[test]
    fn append_keeps_the_length_and_elements_in_the_loop() {
        let mut f = counted(
            "v0: i64, v1: heapref",
            "    v12 = arraylen v1\n    v13 = index v1, v3\n    v14 = arraypush v1, v13\n    v6 = iadd v12, v13",
        );
        licm(&mut f, true);
        f.verify().unwrap();
        assert_eq!(in_loop(&f, |i| matches!(i, MirInst::ArrayLen { .. })), 1);
        assert_eq!(in_loop(&f, |i| matches!(i, MirInst::Index { .. })), 1);
    }

    #[test]
    fn length_moves_past_element_stores() {
        let mut f = counted("v0: i64, v1: heapref", "    v6 = arraylen v1\n    v12 = storeindex v1, v4, v3");
        assert!(licm(&mut f, false) >= 1);
        f.verify().unwrap();
        assert_eq!(in_loop(&f, |i| matches!(i, MirInst::ArrayLen { .. })), 0);
    }

    #[test]
    fn field_read_moves_past_element_stores() {
        let mut f = counted(
            "v0: i64, v1: heapref, v10: heapref",
            "    v6 = heaploadfield v10, 0\n    v12 = storeindex v1, v4, v3",
        );
        assert!(licm(&mut f, true) >= 1);
        f.verify().unwrap();
        assert_eq!(in_loop(&f, |i| matches!(i, MirInst::HeapFieldLoad { .. })), 0);
    }

    #[test]
    fn field_store_keeps_field_reads_in_the_loop() {
        // `v1` may be `v10`.
        let mut f = counted(
            "v0: i64, v1: heapref, v10: heapref",
            "    v6 = heaploadfield v10, 0\n    v12 = heapsetfield.slot v1, v4, 0",
        );
        licm(&mut f, true);
        f.verify().unwrap();
        assert_eq!(in_loop(&f, |i| matches!(i, MirInst::HeapFieldLoad { .. })), 1);
    }

    #[test]
    fn readonly_class_field_read_moves_past_field_stores() {
        // #1560: no store changes a readonly class instance's field.
        let mut f = counted(
            "v0: i64, v1: heapref, v10: heapref",
            "    v6 = heaploadfield v10, 0\n    v12 = heapsetfield.slot v1, v4, 0",
        );
        f.invariant_loads.insert(ValueId(6));
        assert!(licm(&mut f, true) >= 1);
        f.verify().unwrap();
        assert_eq!(in_loop(&f, |i| matches!(i, MirInst::HeapFieldLoad { .. })), 0);
    }

    #[test]
    fn element_read_moves_past_field_stores_not_element_stores() {
        let mut f = counted(
            "v0: i64, v1: heapref, v10: heapref",
            "    v6 = index v1, v3\n    v12 = heapsetfield.slot v10, v4, 0",
        );
        assert!(licm(&mut f, true) >= 1);
        f.verify().unwrap();
        assert_eq!(in_loop(&f, |i| matches!(i, MirInst::Index { .. })), 0);
        // `v10` may be `v1`.
        let mut f = counted(
            "v0: i64, v1: heapref, v10: heapref",
            "    v6 = index v1, v3\n    v12 = storeindex v10, v4, v3",
        );
        licm(&mut f, true);
        f.verify().unwrap();
        assert_eq!(in_loop(&f, |i| matches!(i, MirInst::Index { .. })), 1);
    }

    #[test]
    fn hoisted_code_follows_the_preheaders_own_definitions() {
        // `v1` is defined in the entry block, which is the preheader.
        let src = r#"
func @f(v0: i64) -> f64 {
bb0:
    v1 = fcvt.f64.i64 v0
    v2 = iconst.i64 0
    v3 = iconst.i64 1
    v20 = fconst.f64 bits=0
    jump bb1
bb1:
    v4 = phi.i64 [bb0: v2, bb2: v8]
    v5 = phi.f64 [bb0: v20, bb2: v7]
    v9 = icmp.slt v4, v0
    brif v9, bb2, bb3
bb2:
    v6 = fmul v1, v1
    v7 = fadd v5, v6
    v8 = iadd v4, v3
    jump bb1
bb3:
    return v5
}
"#;
        let mut f = crate::mir::parse_func(src).expect(src);
        assert!(licm(&mut f, true) >= 1);
        f.verify().unwrap();
        let entry = &f.block(f.entry).insts;
        let at = |want: &dyn Fn(&MirInst) -> bool| entry.iter().position(want).expect("in the entry block");
        assert!(at(&|i| matches!(i, MirInst::Cast { .. })) < at(&|i| matches!(i, MirInst::Bin { op: MirBinOp::Mul, .. })));
    }

    #[test]
    fn lir_keeps_literals_in_the_loop() {
        // `v10` feeds an invariant add (`v11`) and a variant one (`v13`).
        let body = "    v10 = fconst.f64 bits=0\n    v11 = fadd v1, v10\n    v12 = fcvt.f64.i64 v4\n    v13 = fadd v12, v10\n    v14 = fadd v11, v13\n    v6 = ftrunc.i64.f64 v14";
        let fadds = |f: &MirFunc| in_loop(f, |i| matches!(i, MirInst::Bin { op: MirBinOp::Add, ty, .. } if ty.is_float()));
        let mut f = counted("v0: i64, v1: f64", body);
        licm(&mut f, false);
        f.verify().unwrap();
        assert_eq!(fadds(&f), 2, "the invariant add moves");
        assert_eq!(in_loop(&f, |i| matches!(i, MirInst::Const { .. })), 1, "an immediate operand stays");
        let mut f = counted("v0: i64, v1: f64", body);
        licm(&mut f, true);
        f.verify().unwrap();
        assert_eq!(fadds(&f), 2);
        assert_eq!(in_loop(&f, |i| matches!(i, MirInst::Const { .. })), 0);
    }

    /// `n` diamonds in a row, each `if v0 { .. }` joining before the next:
    /// the shape a run of `if x == k { s = s + "a"; }` lowers to (#1518).
    fn join_chain(n: usize) -> String {
        let mut text = String::from("func @f(v0: bool) -> i64 {\nbb0:\n    brif v0, bb1, bb2\n");
        for i in 0..n {
            let (arm, join) = (2 * i + 1, 2 * i + 2);
            text.push_str(&format!("bb{arm}:\n    jump bb{join}\n"));
            text.push_str(&format!("bb{join}:\n"));
            if i + 1 < n {
                text.push_str(&format!("    brif v0, bb{}, bb{}\n", join + 1, join + 2));
            } else {
                text.push_str("    v1 = iconst.i64 0\n    return v1\n");
            }
        }
        text.push('}');
        text
    }

    /// #1518: finding loops costs about linear work over a chain of joins.
    /// Each block kept the set of every block that dominates it, so each
    /// join's set held every join before it: a chain 8x longer took ~64x.
    #[test]
    fn natural_loops_about_linear_over_join_chain() {
        let steps = |n: usize| {
            let text = join_chain(n);
            let f = crate::mir::parse_func(&text).expect(&text);
            f.verify().unwrap();
            DOM_STEPS.with(|c| c.set(0));
            assert!(natural_loops(&f).is_empty());
            DOM_STEPS.with(|c| c.get())
        };
        let small = steps(100);
        let large = steps(800);
        // Linear is ~8x.
        assert!(large <= 24 * small.max(1), "800 joins stepped {large}, 100 stepped {small}");
    }

    /// The set-per-block fixpoint `Dominators` replaced: `dom[b]` holds
    /// every block that dominates `b`.
    fn dominator_sets(func: &MirFunc) -> Vec<HashSet<BlockId>> {
        let preds = func.preds();
        let n = func.blocks.len();
        let all: HashSet<BlockId> = (0..n).map(|i| BlockId(i as u32)).collect();
        let mut dom = vec![all; n];
        dom[func.entry.index()] = HashSet::from([func.entry]);
        let mut changed = true;
        while changed {
            changed = false;
            for b in &func.blocks {
                if b.id == func.entry {
                    continue;
                }
                let mut d: Option<HashSet<BlockId>> = None;
                for p in &preds[b.id.index()] {
                    let pd = &dom[p.index()];
                    d = Some(match d {
                        None => pd.clone(),
                        Some(acc) => acc.intersection(pd).copied().collect(),
                    });
                }
                let mut d = d.unwrap_or_default();
                d.insert(b.id);
                if d != dom[b.id.index()] {
                    dom[b.id.index()] = d;
                    changed = true;
                }
            }
        }
        dom
    }

    /// `Dominators` answers as the set fixpoint did, on loops, joins,
    /// blocks nothing branches to, and a cycle nothing enters.
    #[test]
    fn dominators_match_set_fixpoint() {
        let bodies = [
            // A loop with a join inside and a second exit.
            "bb0:\n    jump bb1\nbb1:\n    brif v0, bb2, bb5\nbb2:\n    brif v0, bb3, bb4\nbb3:\n    jump bb4\nbb4:\n    brif v0, bb1, bb5\nbb5:\n    v1 = iconst.i64 0\n    return v1",
            // Nested loops sharing a latch path; the entry is branched to.
            "bb0:\n    brif v0, bb1, bb4\nbb1:\n    brif v0, bb2, bb0\nbb2:\n    brif v0, bb1, bb3\nbb3:\n    jump bb1\nbb4:\n    v1 = iconst.i64 0\n    return v1",
            // An irreducible pair, a block nothing branches to that feeds
            // a join, and a cycle nothing enters.
            "bb0:\n    brif v0, bb1, bb2\nbb1:\n    brif v0, bb2, bb3\nbb2:\n    brif v0, bb1, bb3\nbb3:\n    v1 = iconst.i64 0\n    return v1\nbb4:\n    jump bb3\nbb5:\n    jump bb6\nbb6:\n    brif v0, bb5, bb3\nbb7:\n    jump bb8\nbb8:\n    jump bb9\nbb9:\n    brif v0, bb8, bb4",
        ];
        for body in bodies.iter().map(|b| format!("func @f(v0: bool) -> i64 {{\n{b}\n}}")).chain([join_chain(5)]) {
            let f = crate::mir::parse_func(&body).expect(&body);
            let sets = dominator_sets(&f);
            let dom = Dominators::new(&f, &f.preds());
            let n = f.blocks.len();
            for b in 0..n {
                let b = BlockId(b as u32);
                for d in 0..n {
                    let d = BlockId(d as u32);
                    assert_eq!(dom.dominates(d, b), sets[b.index()].contains(&d), "{d:?} dom {b:?} in\n{body}");
                }
                let reached = sets[b.index()].len() < n || n == 1;
                if reached {
                    // The closest strict dominator has the most dominators.
                    let want = sets[b.index()]
                        .iter()
                        .copied()
                        .filter(|d| *d != b)
                        .max_by_key(|d| sets[d.index()].len());
                    assert_eq!(dom.idom(b), want, "idom of {b:?} in\n{body}");
                }
            }
        }
    }
}
