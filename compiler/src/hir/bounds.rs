//! In-bounds proofs for counted loops.
//!
//! In `while i < len(a) { … i = i + 1 }` (or a bound `n` from
//! `let n = len(a)` just before the loop) and `for i in 0..len(a)`, an
//! `a[i]` reached before anything writes `i` in the iteration is in
//! bounds: `i` starts at a non-negative literal, only grows, and `a` keeps
//! its length while the loop runs (no append, no call that may resize).
//! Those sites get [`HirFlags::IN_BOUNDS`], which lowering turns into an
//! unchecked index. The checker proves the same on source code; this pass
//! sees the shapes inlining and licm leave.

use std::collections::{HashMap, HashSet};

use super::lower::children;
use super::{BinOp, Callee, HirBody, HirFlags, HirId, HirKind, HirPat, IndexKind, Lit, LocalId, MakeKind};
use crate::typechecking::ty::{self, Ty};

/// `body` with proven index sites flagged, or `None` when none are new.
/// `steady(name)` says a call to `name` never resizes an array.
pub fn prove(body: &HirBody, steady: impl Fn(&str) -> bool) -> Option<HirBody> {
    let root = body.root?;
    let mut cx = Cx { body, steady: &steady, sites: Vec::new() };
    cx.walk(root, &Facts::default());
    let mut out = body.clone();
    let mut hits = 0;
    for id in cx.sites {
        let flags = &mut out.exprs[id.0 as usize].flags;
        if !flags.contains(HirFlags::IN_BOUNDS) {
            flags.insert(HirFlags::IN_BOUNDS);
            hits += 1;
        }
    }
    (hits > 0).then_some(out)
}

struct Cx<'a, S> {
    body: &'a HirBody,
    steady: &'a S,
    sites: Vec<HirId>,
}

/// What the statements before a loop establish.
#[derive(Default, Clone)]
struct Facts {
    /// `n` holds `len(a)`, or `a` was filled with `n` elements from empty
    /// (so `len(a) == n` whenever `n > 0`).
    len_of: HashMap<LocalId, LocalId>,
    /// The local holds `0`.
    zero: HashSet<LocalId>,
    /// The local holds an empty array or `Vec`.
    empty: HashSet<LocalId>,
    /// The local holds a non-negative value.
    nonneg: HashSet<LocalId>,
    /// The local holds a value in `0..len(a)` for some array, or a small
    /// non-negative literal: adding two of them cannot overflow.
    small: HashSet<LocalId>,
}

impl Facts {
    /// Drop what a statement writing `w` may change.
    fn forget(&mut self, w: &Writes) {
        if !w.steady {
            self.len_of.clear();
            self.empty.clear();
        }
        self.len_of.retain(|n, a| !w.locals.contains(n) && !w.locals.contains(a));
        self.nonneg.retain(|l| !w.locals.contains(l));
        self.small.retain(|l| !w.locals.contains(l));
        self.zero.retain(|l| !w.locals.contains(l));
        self.empty.retain(|l| !w.locals.contains(l));
    }
}

impl<S: Fn(&str) -> bool> Cx<'_, S> {
    /// Prove the loops under `id`, which runs with `facts` holding
    /// throughout.
    fn walk(&mut self, id: HirId, facts: &Facts) {
        let HirKind::Block { stmts, tail } = &self.body.expr(id).kind else {
            for k in children(self.body, id) {
                self.walk(k, facts);
            }
            return;
        };
        let mut facts = facts.clone();
        for &s in stmts.iter().chain(tail) {
            let counted = self.statement(s, &facts);
            // Inside `s`, only what `s` itself cannot change still holds.
            let w = self.writes(s);
            let mut inner = facts.clone();
            inner.forget(&w);
            match counted {
                // A counted loop's body also starts with its index in
                // `0..len`; the rest of the loop is its test.
                Some((then, i)) => {
                    let mut at_top = inner.clone();
                    at_top.nonneg.insert(i);
                    at_top.small.insert(i);
                    self.walk(then, &at_top);
                }
                None => {
                    for k in children(self.body, s) {
                        self.walk(k, &inner);
                    }
                }
            }
            self.learn(s, &w, &mut facts);
        }
    }

    /// Prove the sites of the loop at `s`, if it is one, and return its
    /// body and index.
    fn statement(&mut self, s: HirId, facts: &Facts) -> Option<(HirId, LocalId)> {
        match &self.body.expr(s).kind {
            HirKind::Loop { body } => self.while_loop(*body, facts),
            HirKind::ForIn { pat: HirPat::Bind(i), iterable, body, .. } => self.for_range(*i, *iterable, *body, facts),
            _ => None,
        }
    }

    /// `loop { if i < bound { then } else { break } }`.
    fn while_loop(&mut self, loop_body: HirId, facts: &Facts) -> Option<(HirId, LocalId)> {
        let body = self.body;
        let HirKind::Block { stmts, tail: None } = &body.expr(loop_body).kind else { return None };
        let [only] = stmts.as_slice() else { return None };
        let HirKind::If { cond, then, els: Some(e) } = body.expr(*only).kind else { return None };
        if !matches!(body.expr(e).kind, HirKind::Break) {
            return None;
        }
        let HirKind::Bin { op: BinOp::Lt, lhs, rhs } = body.expr(cond).kind else { return None };
        let HirKind::Local(i) = body.expr(lhs).kind else { return None };
        if !facts.nonneg.contains(&i) || body.local(i).captured {
            return None;
        }
        let arr = self.bound_array(rhs, facts)?;
        let w = self.writes(loop_body);
        if !w.steady || w.locals.contains(&arr) || self.bound_written(rhs, &w) || !self.only_grows(loop_body, i, facts, &w) {
            return None;
        }
        self.mark_until_write(then, i, arr);
        Some((then, i))
    }

    /// `for i in lo..bound` with `lo` a non-negative literal.
    fn for_range(&mut self, i: LocalId, iterable: HirId, loop_body: HirId, facts: &Facts) -> Option<(HirId, LocalId)> {
        let body = self.body;
        let HirKind::Make { kind: MakeKind::Range { inclusive: false }, args } = &body.expr(iterable).kind else { return None };
        let [lo, hi] = args.as_slice() else { return None };
        if !matches!(body.expr(*lo).kind, HirKind::Lit(Lit::Int(v)) if v >= 0) || body.local(i).captured {
            return None;
        }
        if !body.local(i).ty.as_ref().is_some_and(|t| matches!(ty::strip_readonly(t), Ty::Con(n) if n == "int")) {
            return None;
        }
        let arr = self.bound_array(*hi, facts)?;
        let w = self.writes(loop_body);
        // The range is evaluated once, so only `a`'s length matters; a
        // write to `i` might carry into the next iteration.
        if !w.steady || w.locals.contains(&arr) || w.locals.contains(&i) {
            return None;
        }
        self.mark_until_write(loop_body, i, arr);
        Some((loop_body, i))
    }

    /// The array whose length `bound` is.
    fn bound_array(&self, bound: HirId, facts: &Facts) -> Option<LocalId> {
        let body = self.body;
        match &body.expr(bound).kind {
            HirKind::Call { callee: Callee::Named { name, .. }, args } if name == "len" && length_of(body, args) => match body.expr(args[0]).kind {
                HirKind::Local(a) if !body.local(a).captured => Some(a),
                _ => None,
            },
            HirKind::Local(n) if !body.local(*n).captured => facts.len_of.get(n).copied().filter(|a| !body.local(*a).captured),
            _ => None,
        }
    }

    fn bound_written(&self, bound: HirId, w: &Writes) -> bool {
        matches!(self.body.expr(bound).kind, HirKind::Local(n) if w.locals.contains(&n))
    }

    /// Every write to `i` in the loop is `i = i + k` with `k` a positive
    /// literal or a small local the loop does not write: `i < len(a)` at
    /// the test, so the sum cannot overflow.
    fn only_grows(&self, loop_body: HirId, i: LocalId, facts: &Facts, w: &Writes) -> bool {
        let body = self.body;
        let mut ok = true;
        visit(body, loop_body, &mut |id| match &body.expr(id).kind {
            HirKind::Assign { place, value } if matches!(body.expr(*place).kind, HirKind::Local(l) if l == i) => {
                ok &= match body.expr(*value).kind {
                    HirKind::Bin { op: BinOp::IntAdd, lhs, rhs } => {
                        let me = |x: HirId| matches!(body.expr(x).kind, HirKind::Local(l) if l == i);
                        let step = |x: HirId| match body.expr(x).kind {
                            HirKind::Lit(Lit::Int(k)) => k > 0,
                            HirKind::Local(s) => s != i && facts.small.contains(&s) && !w.locals.contains(&s),
                            _ => false,
                        };
                        (me(lhs) && step(rhs)) || (step(lhs) && me(rhs))
                    }
                    _ => false,
                };
            }
            HirKind::Let { local, .. } if *local == i => ok = false,
            HirKind::Append { base, .. } if matches!(body.expr(*base).kind, HirKind::Local(l) if l == i) => ok = false,
            HirKind::Clear(ls) if ls.contains(&i) => ok = false,
            _ => {}
        });
        ok
    }

    /// Flag `a[i]` in the statements of `block` up to the first that
    /// writes `i`.
    fn mark_until_write(&mut self, block: HirId, i: LocalId, arr: LocalId) {
        let body = self.body;
        let stmts: Vec<HirId> = match &body.expr(block).kind {
            HirKind::Block { stmts, tail } => stmts.iter().chain(tail).copied().collect(),
            _ => vec![block],
        };
        for s in stmts {
            if self.writes(s).locals.contains(&i) {
                break;
            }
            let mut found = Vec::new();
            visit_now(body, s, &mut |id| {
                if let HirKind::Index { base, index, kind: IndexKind::Array } = body.expr(id).kind
                    && matches!(body.expr(base).kind, HirKind::Local(a) if a == arr)
                    && matches!(body.expr(index).kind, HirKind::Local(l) if l == i)
                {
                    found.push(id);
                }
            });
            self.sites.extend(found);
        }
    }

    /// Update `facts` past statement `s`.
    fn learn(&self, s: HirId, w: &Writes, facts: &mut Facts) {
        let body = self.body;
        let filled = self.fill(s, w, facts);
        facts.forget(w);
        if let Some((n, a)) = filled {
            facts.len_of.insert(n, a);
        }
        let (local, value) = match body.expr(s).kind {
            HirKind::Let { local, init: Some(v) } => (local, v),
            HirKind::Assign { place, value } => match body.expr(place).kind {
                HirKind::Local(l) => (l, value),
                _ => return,
            },
            _ => return,
        };
        let small = |id: HirId, f: &Facts| match body.expr(id).kind {
            HirKind::Lit(Lit::Int(v)) => (0..1 << 31).contains(&v),
            HirKind::Local(l) => f.small.contains(&l),
            _ => false,
        };
        match &body.expr(value).kind {
            HirKind::Lit(Lit::Int(v)) if *v >= 0 => {
                facts.nonneg.insert(local);
                if *v == 0 {
                    facts.zero.insert(local);
                }
                if small(value, facts) {
                    facts.small.insert(local);
                }
            }
            HirKind::Local(l) if *l != local => {
                if facts.nonneg.contains(l) {
                    facts.nonneg.insert(local);
                }
                if facts.small.contains(l) {
                    facts.small.insert(local);
                }
            }
            HirKind::Bin { op: BinOp::IntAdd, lhs, rhs } if small(*lhs, facts) && small(*rhs, facts) => {
                facts.nonneg.insert(local);
            }
            HirKind::Call { callee: Callee::Named { name, .. }, args } if name == "Vec::with_capacity" || name == "Vec::new" && args.is_empty() => {
                facts.empty.insert(local);
            }
            HirKind::Make { kind: MakeKind::Array, args } if args.is_empty() => {
                facts.empty.insert(local);
            }
            HirKind::Call { callee: Callee::Named { name, .. }, args } if name == "len" && length_of(body, args) => {
                if let HirKind::Local(a) = body.expr(args[0]).kind
                    && a != local
                {
                    facts.len_of.insert(local, a);
                }
            }
            _ => {}
        }
    }

    /// `while i < n { a.push(x); i = i + 1 }` from `i == 0` and an empty
    /// `a`: afterwards `a` holds `n` elements, when `n > 0`.
    fn fill(&self, s: HirId, w: &Writes, facts: &Facts) -> Option<(LocalId, LocalId)> {
        let body = self.body;
        let HirKind::Loop { body: lb } = body.expr(s).kind else { return None };
        let HirKind::Block { stmts, tail: None } = &body.expr(lb).kind else { return None };
        let [only] = stmts.as_slice() else { return None };
        let HirKind::If { cond, then, els: Some(e) } = body.expr(*only).kind else { return None };
        if !matches!(body.expr(e).kind, HirKind::Break) {
            return None;
        }
        let HirKind::Bin { op: BinOp::Lt, lhs, rhs } = body.expr(cond).kind else { return None };
        let (HirKind::Local(i), HirKind::Local(n)) = (&body.expr(lhs).kind, &body.expr(rhs).kind) else { return None };
        let (i, n) = (*i, *n);
        if i == n || !facts.zero.contains(&i) || w.locals.contains(&n) || body.local(n).captured {
            return None;
        }
        let HirKind::Block { stmts, tail: None } = &body.expr(then).kind else { return None };
        let [x, y] = stmts.as_slice() else { return None };
        let bump = |id: HirId| match body.expr(id).kind {
            HirKind::Assign { place, value } if matches!(body.expr(place).kind, HirKind::Local(l) if l == i) => matches!(
                body.expr(value).kind,
                HirKind::Bin { op: BinOp::IntAdd, lhs, rhs }
                    if matches!(body.expr(lhs).kind, HirKind::Local(l) if l == i) && matches!(body.expr(rhs).kind, HirKind::Lit(Lit::Int(1)))
            ),
            _ => false,
        };
        let push = |id: HirId| match &body.expr(id).kind {
            HirKind::Call { callee: Callee::Method { name }, args } if name == "push" => match args.as_slice() {
                [v, x] => match body.expr(*v).kind {
                    HirKind::Local(a) if a != i && a != n && facts.empty.contains(&a) && !body.local(a).captured => {
                        let wx = self.writes(*x);
                        let mut leaves = false;
                        visit(body, *x, &mut |k| {
                            leaves |= matches!(body.expr(k).kind, HirKind::Break | HirKind::Continue | HirKind::Return(_));
                        });
                        (wx.steady && wx.locals.is_empty() && !leaves).then_some(a)
                    }
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        };
        let a = match (push(*x), push(*y)) {
            (Some(a), None) if bump(*y) => a,
            (None, Some(a)) if bump(*x) => a,
            _ => return None,
        };
        (!w.locals.contains(&a)).then_some((n, a))
    }

    /// Locals `id` writes, and whether it leaves every length alone.
    fn writes(&self, id: HirId) -> Writes {
        let body = self.body;
        let mut w = Writes { locals: HashSet::new(), steady: true };
        visit(body, id, &mut |k| match &body.expr(k).kind {
            HirKind::Let { local, .. } => {
                w.locals.insert(*local);
            }
            HirKind::LetPat { pat, .. } => binds(pat, &mut w.locals),
            HirKind::Match { arms, .. } => arms.iter().for_each(|a| binds(&a.pat, &mut w.locals)),
            HirKind::ForIn { pat, .. } => binds(pat, &mut w.locals),
            HirKind::Assign { place, .. } => {
                if let HirKind::Local(l) = body.expr(*place).kind {
                    w.locals.insert(l);
                }
            }
            HirKind::Append { .. } | HirKind::Yield { .. } | HirKind::Resume { .. } => w.steady = false,
            HirKind::Clear(ls) => w.locals.extend(ls),
            HirKind::Call { callee: Callee::Named { name, .. }, args } => {
                w.steady &= (name == "len" || name == "capacity") && length_of(body, args) || (self.steady)(name);
            }
            HirKind::Call { .. } => w.steady = false,
            HirKind::Builtin { op, .. } => {
                w.steady &= matches!(op, super::Builtin::Panic | super::Builtin::TypeOf | super::Builtin::Readonly | super::Builtin::Default);
            }
            _ => {}
        });
        w
    }
}

struct Writes {
    locals: HashSet<LocalId>,
    steady: bool,
}

/// One argument, an array, `Vec` or string: the structural length.
fn length_of(body: &HirBody, args: &[HirId]) -> bool {
    let [a] = args else { return false };
    body.expr(*a).ty.as_ref().is_some_and(|t| {
        let t = ty::strip_readonly(t);
        matches!(t, Ty::Array { .. }) || ty::vec_element_ty(t).is_some() || matches!(t, Ty::Con(n) if n == "string")
    })
}

fn binds(pat: &HirPat, out: &mut HashSet<LocalId>) {
    match pat {
        HirPat::Bind(l) => {
            out.insert(*l);
        }
        HirPat::Variant {
            fields: super::HirPatFields::Tuple(ps),
            ..
        }
        | HirPat::Tuple(ps) => ps.iter().for_each(|p| binds(p, out)),
        HirPat::Variant {
            fields: super::HirPatFields::Record(fs),
            ..
        }
        | HirPat::Record(fs) => fs.iter().for_each(|(_, p)| binds(p, out)),
        _ => {}
    }
}

/// Every node under `id`, nested function bodies included.
fn visit(body: &HirBody, id: HirId, f: &mut impl FnMut(HirId)) {
    f(id);
    for k in children(body, id) {
        visit(body, k, f);
    }
}

/// The nodes under `id` that run when it does: not inside a lambda or a
/// `defer`, which run later.
fn visit_now(body: &HirBody, id: HirId, f: &mut impl FnMut(HirId)) {
    if matches!(body.expr(id).kind, HirKind::Lambda { .. } | HirKind::Defer { .. }) {
        return;
    }
    f(id);
    for k in children(body, id) {
        visit_now(body, k, f);
    }
}

#[cfg(test)]
#[path = "bounds.tests.rs"]
mod tests;
