//! Loop views (#2351): a loop that writes through a nested path,
//! `f.blocks[b].insts[i] = …`, makes every object on the path unique before
//! each write, and reads the whole path again for each read. Where nothing
//! in the loop can give one of those objects a second name or put another
//! object on the path, the path names the same unique objects on every
//! iteration, and the loop gets it bound once before it, as `let v = mut
//! f.blocks[b].insts` written there would bind it: the rewrite then makes
//! the path unique once per loop entry, and the loop writes the view in
//! place.
//!
//! The path `r.s1…sk` qualifies in loop `L` when:
//!
//! - its root `r` and every index in it are locals `L` does not assign and
//!   nothing captures (or literals);
//! - no write in `L` changes an object above the path's last one (other
//!   than a store into a field the path does not take), and no write
//!   through another name may reach the path;
//! - nothing in `L` shares an object on the path (a store, a binding the
//!   rewrite shares, a call that keeps or returns it), and no call is handed
//!   an object above the last one with any use but a read;
//! - `L` has no `yield`, `resume`, closure or `defer`;
//! - a root the body does not own (a parameter, a borrow) is reached by
//!   known calls only, which touch no `static` and switch no task: a
//!   `static` may be the caller's place;
//! - evaluating the path cannot fail before the loop would fail the same
//!   way: its last index step is read first thing on entry (the loop's
//!   test or iterable), after nothing but locals and literals.
//!
//! The rewrite of the viewed body runs again, and the view stands only if it
//! draws no diagnostic the body did not have (E0149 checks the borrow as
//! for a written one).

use std::collections::HashSet;

use super::{borrow_field, cow_ty, host_short, named_value, owned, pack_site, Before, CallLookup, Facts, Plan, Write};
use crate::hir::{Callee, HirBody, HirId, HirKind, HirLocal, IndexKind, Lit, LocalId, LocalKind};
use crate::typechecking::infer::Checker;
use crate::typechecking::ty::{self as ty, strip_readonly, Ty};
use common::EffectFlags;

/// Rounds of views per body: each round views the outermost loops it can,
/// and the next sees the loops inside them with the views as roots.
pub(super) const ROUNDS: usize = 4;

/// Effects of a call during which another task or a finalizer may run.
const SWITCHES: u16 =
    EffectFlags::UNKNOWN | EffectFlags::YIELD | EffectFlags::THREAD | EffectFlags::GC | EffectFlags::ATTACH_PARK | EffectFlags::SUSPEND | EffectFlags::FFI;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Key {
    Lit(i64),
    Local(LocalId),
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum Step {
    Field(String),
    /// An element: by a literal or a local, `None` for any other index.
    Elem(Option<Key>),
}

impl Step {
    fn may_equal(&self, other: &Step) -> bool {
        match (self, other) {
            (Step::Field(a), Step::Field(b)) => a == b,
            (Step::Elem(Some(Key::Lit(a))), Step::Elem(Some(Key::Lit(b)))) => a == b,
            (Step::Elem(_), Step::Elem(_)) => true,
            _ => false,
        }
    }
}

/// A place: the node below its fields and elements, its steps, and the
/// node that reads each step (`nodes[j]`: after `j + 1` steps).
struct Place {
    root: HirId,
    steps: Vec<Step>,
    nodes: Vec<HirId>,
}

fn place(body: &HirBody, v: HirId) -> Place {
    let (mut steps, mut nodes, mut at) = (Vec::new(), Vec::new(), v);
    loop {
        match &body.expr(at).kind {
            HirKind::Field { base, name } => {
                steps.push(Step::Field(name.clone()));
                nodes.push(at);
                at = *base;
            }
            HirKind::Index { base, index, kind: IndexKind::Array } => {
                let key = match &body.expr(*index).kind {
                    HirKind::Lit(Lit::Int(i)) => Some(Key::Lit(*i)),
                    HirKind::Local(l) => Some(Key::Local(*l)),
                    _ => None,
                };
                steps.push(Step::Elem(key));
                nodes.push(at);
                at = *base;
            }
            _ => break,
        }
    }
    steps.reverse();
    nodes.reverse();
    Place { root: at, steps, nodes }
}

/// The nodes whose value `v` may be: through a block's tail, both arms of
/// an `if`, each `match` arm, a named argument.
fn value_places(body: &HirBody, v: HirId) -> Vec<HirId> {
    let (mut out, mut work) = (Vec::new(), vec![v]);
    while let Some(at) = work.pop() {
        match &body.expr(at).kind {
            HirKind::Block { tail: Some(t), .. } => work.push(*t),
            HirKind::Block { tail: None, .. } => {}
            HirKind::If { then, els, .. } => work.extend(std::iter::once(*then).chain(*els)),
            HirKind::Match { arms, .. } => work.extend(arms.iter().map(|a| a.body)),
            HirKind::Named { value, .. } => work.push(*value),
            _ => out.push(at),
        }
    }
    out
}

/// Which object of the viewed path a value may be.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// None of them.
    None,
    /// The one after this many steps at most (`0`: the root's).
    Depth(usize),
    /// Any of them.
    Any,
}

/// The path a loop gets a view of.
struct View {
    lp: HirId,
    root: LocalId,
    steps: Vec<Step>,
    node: HirId,
}

/// One round: the body with a view bound before each outermost loop that
/// qualifies (module docs), or `None` when no loop does.
pub(super) fn round(checker: &Checker, body: &HirBody, f: &Facts<'_>, plan: &Plan, lookup: &CallLookup<'_>) -> Option<HirBody> {
    if f.writes.is_empty() {
        return None;
    }
    let mut loops: Vec<HirId> = (0..body.exprs.len() as u32)
        .map(HirId)
        .filter(|&id| f.order[id.0 as usize] != u32::MAX && matches!(body.expr(id).kind, HirKind::Loop { .. } | HirKind::ForIn { .. }))
        .collect();
    loops.sort_by_key(|&l| f.order(l));
    let cx = Cx { checker, body, f, plan, lookup };
    let mut views: Vec<View> = Vec::new();
    for lp in loops {
        if views.iter().any(|v| f.contains(v.lp, lp)) {
            continue;
        }
        if let Some(v) = cx.view(lp) {
            views.push(v);
        }
    }
    if views.is_empty() {
        return None;
    }
    let mut out = body.clone();
    let mut before = Before::default();
    for v in &views {
        let init = super::dup(&mut out, v.node);
        let e = body.expr(v.node);
        let (ty, span) = (e.ty.clone(), e.span);
        let local = LocalId(out.locals.len() as u32);
        out.locals.push(HirLocal { name: format!("$view{}", local.0), ty, kind: LocalKind::Let, captured: false, borrow: true });
        let mut hits = Vec::new();
        body.visit_ids(v.lp, &mut |id| {
            if cx.names(id, v) {
                hits.push(id);
            }
        });
        for id in hits {
            let x = &mut out.exprs[id.0 as usize];
            x.kind = HirKind::Local(local);
            x.node = None;
            x.flags = Default::default();
        }
        let stmt = out.push_new(HirKind::Let { local, init: Some(init) }, Some(ty::unit()), span);
        before.add(f.statement_of(v.lp), stmt);
    }
    before.apply(&mut out);
    Some(out)
}

/// Whether `plan` shares a view this module bound (a local past the
/// first `base` that borrows): a write through a view makes nothing
/// unique, so a view must have no second name.
pub(super) fn shares_view(plan: &Plan, body: &HirBody, base: usize) -> bool {
    plan.share.iter().any(|&(v, _)| matches!(body.expr(v).kind, HirKind::Local(l) if l.0 as usize >= base && body.local(l).borrow))
}

struct Cx<'a, 'b> {
    checker: &'a Checker,
    body: &'a HirBody,
    f: &'a Facts<'b>,
    plan: &'a Plan,
    lookup: &'a CallLookup<'a>,
}

impl Cx<'_, '_> {
    /// Whether `id` reads exactly the viewed place.
    fn names(&self, id: HirId, v: &View) -> bool {
        if !matches!(self.body.expr(id).kind, HirKind::Field { .. } | HirKind::Index { .. }) {
            return false;
        }
        let p = place(self.body, id);
        matches!(self.body.expr(p.root).kind, HirKind::Local(l) if l == v.root) && p.steps == v.steps
    }

    /// The deepest path a write in `lp` goes through that qualifies.
    fn view(&self, lp: HirId) -> Option<View> {
        let (body, f) = (self.body, self.f);
        // Run before the loop's own statement and nothing else of it.
        if f.statement_of(lp).1 != lp {
            return None;
        }
        if body.any(lp, &|id| matches!(body.expr(id).kind, HirKind::Yield { .. } | HirKind::Resume { .. } | HirKind::Lambda { .. } | HirKind::Defer { .. })) {
            return None;
        }
        let writes: Vec<Write> = f.writes.iter().filter(|w| f.contains(lp, w.at)).copied().collect();
        for w in &writes {
            let p = place(body, w.path);
            let HirKind::Local(r) = body.expr(p.root).kind else { continue };
            for k in (1..=p.steps.len()).rev() {
                let steps = &p.steps[..k];
                if steps.contains(&Step::Elem(None)) {
                    continue;
                }
                let node = p.nodes[k - 1];
                if !cow_ty(self.checker, body.expr(node).ty.as_ref())
                    || super::open_param(self.checker, body.expr(node).ty.as_ref())
                    || pack_site(self.checker, body, node)
                    || p.nodes[..k].iter().any(|&n| borrow_field(self.checker, body, n))
                {
                    continue;
                }
                if self.fits(lp, r, steps, &writes) {
                    return Some(View { lp, root: r, steps: steps.to_vec(), node });
                }
            }
        }
        None
    }

    fn fits(&self, lp: HirId, r: LocalId, steps: &[Step], writes: &[Write]) -> bool {
        let (body, f) = (self.body, self.f);
        let k = steps.len();
        if !self.steady(lp, r) || steps.iter().any(|s| matches!(s, Step::Elem(Some(Key::Local(i))) if !self.steady(lp, *i))) {
            return false;
        }
        let foreign = self.foreign(r);
        if !writes.iter().all(|w| self.write_fits(r, steps, foreign, w)) {
            return false;
        }
        // Nothing gives an object on the path a second name.
        if self.plan.share.iter().any(|&(v, _)| f.contains(lp, v) && self.reach(r, steps, foreign, v) != Reach::None) {
            return false;
        }
        let captures = self.plan.share_before.iter().chain(&self.plan.unique_before).map(|&(at, l)| (at.1, l));
        if self.plan.share_after.iter().copied().chain(captures).any(|(at, l)| f.contains(lp, at) && self.may_alias(l, r)) {
            return false;
        }
        // Calls: an object above the last is only read, the last is not kept.
        for (&c, u) in &f.calls {
            if !f.contains(lp, c) {
                continue;
            }
            let HirKind::Call { callee, args } = &body.expr(c).kind else { continue };
            if args.len() > 64 || foreign && !self.known(callee, args) {
                return false;
            }
            for (j, &a) in args.iter().enumerate() {
                let bit = 1u64 << j;
                let used = (u.written | u.kept | u.returned | u.opaque | u.inner) & bit != 0;
                let keeps = (u.kept | u.returned | u.opaque) & bit != 0;
                match self.reach(r, steps, foreign, named_value(body, a)) {
                    Reach::None => {}
                    Reach::Any => {
                        if used {
                            return false;
                        }
                    }
                    Reach::Depth(d) => {
                        if d < k && used || d == k && keeps {
                            return false;
                        }
                    }
                }
            }
        }
        // The path up to its last element is read first thing on entry.
        match steps.iter().rposition(|s| matches!(s, Step::Elem(_))) {
            Some(last) => self.entry_reads(lp, r, &steps[..=last]),
            None => true,
        }
    }

    /// Whether write `w` leaves every object on the path where it is.
    fn write_fits(&self, r: LocalId, steps: &[Step], foreign: bool, w: &Write) -> bool {
        let body = self.body;
        for pv in value_places(body, w.path) {
            let p = place(body, pv);
            match &body.expr(p.root).kind {
                HirKind::Local(x) if *x == r => {
                    let d = p.steps.len();
                    if d < steps.len() && p.steps.iter().zip(steps).all(|(a, b)| a.may_equal(b)) && !self.other_slot(w.at, &steps[d]) {
                        return false;
                    }
                }
                HirKind::Local(x) => {
                    if self.may_alias(*x, r) {
                        return false;
                    }
                }
                HirKind::Global { .. } => {
                    if foreign {
                        return false;
                    }
                }
                HirKind::Call { .. } | HirKind::Make { .. } | HirKind::Lit(_) => {}
                _ => return false,
            }
        }
        true
    }

    /// Whether `at` stores into a slot of an object on the path other
    /// than the one the path's `next` step takes.
    fn other_slot(&self, at: HirId, next: &Step) -> bool {
        let HirKind::Assign { place: p, .. } = &self.body.expr(at).kind else { return false };
        match (&self.body.expr(*p).kind, next) {
            (HirKind::Field { name, .. }, Step::Field(n)) => name != n,
            (HirKind::Index { index, .. }, Step::Elem(Some(Key::Lit(j)))) => {
                matches!(self.body.expr(*index).kind, HirKind::Lit(Lit::Int(i)) if i != *j)
            }
            _ => false,
        }
    }

    /// Which object on the path `v` may evaluate to.
    fn reach(&self, r: LocalId, steps: &[Step], foreign: bool, v: HirId) -> Reach {
        let body = self.body;
        let mut out = Reach::None;
        // A scalar, a string or a function is no object.
        if !super::may_hold_objects(body.expr(v).ty.as_ref()) {
            return out;
        }
        for pv in value_places(body, v) {
            let p = place(body, pv);
            match &body.expr(p.root).kind {
                HirKind::Local(x) if *x == r => {
                    let d = p.steps.len();
                    if d <= steps.len() && p.steps.iter().zip(steps).all(|(a, b)| a.may_equal(b)) {
                        out = match out {
                            Reach::Depth(e) => Reach::Depth(e.min(d)),
                            _ => Reach::Depth(d),
                        };
                    }
                }
                HirKind::Local(x) => {
                    if self.may_alias(*x, r) {
                        return Reach::Any;
                    }
                }
                HirKind::Global { .. } => {
                    if foreign {
                        return Reach::Any;
                    }
                }
                // A call's arguments are checked at the call; a new object
                // or a literal is no object on the path.
                HirKind::Call { .. } | HirKind::Make { .. } | HirKind::Lit(_) => {}
                _ => return Reach::Any,
            }
        }
        out
    }

    /// Whether `l` holds the same value on every iteration of `lp`: not
    /// assigned or bound in it, not captured.
    fn steady(&self, lp: HirId, l: LocalId) -> bool {
        let f = self.f;
        self.body.local(l).kind != LocalKind::Capture
            && !f.captured.contains(&l)
            && f.inits.get(&l).is_none_or(|vs| vs.iter().all(|&v| !f.contains(lp, v)))
            && f.bound.get(&l).is_none_or(|&(_, binder)| !f.contains(lp, binder))
    }

    /// Whether the body does not own `r`'s object: a parameter's (the
    /// caller's place, which may be a `static`), a borrow's, or one read
    /// from a place.
    fn foreign(&self, r: LocalId) -> bool {
        let local = self.body.local(r);
        local.borrow || matches!(local.kind, LocalKind::Param | LocalKind::Capture | LocalKind::Pattern) || !owned(self.f, self.body, r, &self.plan.inout)
    }

    /// The locals `l` may have been read from, `l` included.
    fn sources(&self, l: LocalId) -> HashSet<LocalId> {
        let (body, f) = (self.body, self.f);
        let mut seen = HashSet::new();
        let mut work = vec![l];
        while let Some(x) = work.pop() {
            if !seen.insert(x) {
                continue;
            }
            let srcs = f.inits.get(&x).into_iter().flatten().chain(f.bound.get(&x).map(|(s, _)| s));
            for &s in srcs {
                for pv in value_places(body, s) {
                    if let HirKind::Local(m) = body.expr(place(body, pv).root).kind {
                        work.push(m);
                    }
                }
            }
        }
        seen
    }

    /// Whether `x` may name an object on `r`'s path: read from the same
    /// local, or both from places the body cannot trace (two parameters
    /// may be given one object).
    fn may_alias(&self, x: LocalId, r: LocalId) -> bool {
        if x == r {
            return true;
        }
        let (sx, sr) = (self.sources(x), self.sources(r));
        let untraced = |l: &LocalId| matches!(self.body.local(*l).kind, LocalKind::Param | LocalKind::Capture);
        sx.iter().any(|l| sr.contains(l)) || sx.iter().any(untraced) && sr.iter().any(untraced)
    }

    /// Whether a call touches no `static` and lets no other task or
    /// finalizer run: a `Vec`'s own methods, a host native that does not
    /// switch, a function whose summary says so.
    fn known(&self, callee: &Callee, args: &[HirId]) -> bool {
        let body = self.body;
        let switches = |e: EffectFlags| e.bits() & SWITCHES != 0;
        let summary = (self.lookup)(callee, args);
        let recv_vec = matches!(callee, Callee::Method { .. })
            && args.first().is_some_and(|r| {
                let ty = body.expr(*r).ty.as_ref();
                super::is_vec(ty) || matches!(ty.map(strip_readonly), Some(Ty::Array { .. }))
            });
        if recv_vec && summary.is_none() {
            return !args.iter().any(|&a| matches!(body.expr(a).ty.as_ref().map(strip_readonly), Some(Ty::Fun(..))));
        }
        if summary.is_none()
            && let Callee::Named { name, .. } = callee
            && let Some(short) = host_short(name)
        {
            return common::HOST_NATIVES.iter().find(|n| n.name == short).is_some_and(|n| !switches(n.effects));
        }
        summary.is_some_and(|s| !s.statics && !switches(s.flags))
    }

    /// Whether `lp`'s entry reads the place `r.need` before it runs
    /// anything but reads of locals and literals.
    fn entry_reads(&self, lp: HirId, r: LocalId, need: &[Step]) -> bool {
        let start = match &self.body.expr(lp).kind {
            HirKind::Loop { body } => *body,
            HirKind::ForIn { iterable, .. } => *iterable,
            _ => return false,
        };
        self.entry(start, r, need) == Entry::Found
    }

    fn entry(&self, n: HirId, r: LocalId, need: &[Step]) -> Entry {
        let body = self.body;
        let first = |x: HirId| match self.entry(x, r, need) {
            Entry::Found => Entry::Found,
            _ => Entry::Stop,
        };
        let seq = |xs: &mut dyn Iterator<Item = HirId>| {
            for x in xs {
                match self.entry(x, r, need) {
                    Entry::Pure => continue,
                    other => return other,
                }
            }
            Entry::Pure
        };
        match &body.expr(n).kind {
            HirKind::Field { base, .. } | HirKind::Index { base, .. } => {
                let p = place(body, n);
                if matches!(body.expr(p.root).kind, HirKind::Local(x) if x == r) && p.steps.len() >= need.len() && p.steps[..need.len()] == *need {
                    return Entry::Found;
                }
                // A field read of a local cannot fail; an element read can.
                match &body.expr(n).kind {
                    HirKind::Field { .. } => self.entry(*base, r, need),
                    _ => first(*base),
                }
            }
            HirKind::Local(_) | HirKind::Lit(_) | HirKind::Let { init: None, .. } => Entry::Pure,
            HirKind::Let { init: Some(v), .. } | HirKind::Named { value: v, .. } | HirKind::Loop { body: v } => self.entry(*v, r, need),
            HirKind::Block { stmts, tail } => seq(&mut stmts.iter().copied().chain(*tail)),
            HirKind::If { cond: x, .. }
            | HirKind::Logic { lhs: x, .. }
            | HirKind::ForIn { iterable: x, .. }
            | HirKind::Match { scrutinee: x, .. }
            | HirKind::Un { operand: x, .. }
            | HirKind::Cast { value: x } => first(*x),
            // The operator itself may fail once both sides are read.
            HirKind::Bin { lhs, rhs, .. } => match self.entry(*lhs, r, need) {
                Entry::Pure => first(*rhs),
                other => other,
            },
            HirKind::Call { callee, args } if !matches!(callee, Callee::Value(_)) => match seq(&mut args.iter().copied()) {
                Entry::Pure => Entry::Stop,
                other => other,
            },
            HirKind::Make { args, .. } => match seq(&mut args.iter().copied()) {
                Entry::Pure => Entry::Stop,
                other => other,
            },
            _ => Entry::Stop,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Entry {
    /// The place is read here.
    Found,
    /// Only locals and literals were read: what follows runs next.
    Pure,
    /// Something else runs first.
    Stop,
}

#[cfg(test)]
#[path = "cow_view.tests.rs"]
mod tests;
