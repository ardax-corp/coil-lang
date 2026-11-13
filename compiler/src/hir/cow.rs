//! Copy-on-write: value semantics for class instances, `Vec`s and arrays.
//!
//! Every place (a local, a field, an element) that holds a class instance
//! or an array owns its value: a write through one place is never seen
//! through another. Objects are still passed by reference; this rewrite
//! keeps the sharing honest with the `Cow` instruction ([`Builtin::Cow`]):
//!
//! - *share* where a second place starts to name an object another place
//!   keeps naming: a store of a place read (into a field, an element, a new
//!   object, a static), a return of a field or a parameter, a capture, and a
//!   binding (`let b = a.items`, a `for` or `match` binding) that is written
//!   through, or whose source is written while it lives;
//! - *unique* before a write through a place: each object on the path from
//!   the place's root is made writable top-down and a copy is written back,
//!   `a = unique(a); a.b = unique(a.b); a.b.c = v`. Unique copies an object
//!   only when it was shared.
//!
//! A parameter a body writes through is the caller's place (an inferred
//! `inout`, from the effects summary's `mutates`): the caller makes the
//! argument unique and writes it back before the call, so the callee writes
//! it in place, and stores of the parameter itself copy it eagerly. A
//! coroutine's parameters are its own values: it writes them on `resume`. Class
//! instances whose type has a `drop` are identities the runtime never copies; a
//! `readonly class` is never written, so it needs neither op.
//!
//! The rewrite runs once per module, after the effects fixed point and
//! before typed inlining, so spliced callees carry their ops.

use std::collections::{HashMap, HashSet};

use super::escape::CallLookup;
use super::{is_vec, Summary, VEC_MUTATORS};
use crate::hir::query::pure_index;
use crate::hir::{Builtin, Callee, HirBody, HirFlags, HirId, HirKind, HirModule, HirPat, IndexKind, Lit, LocalId, LocalKind, MakeKind, Span};
use crate::typechecking::infer::{Checker, ForInKind};
use reporting::{ErrorCode, Message};
use crate::typechecking::ty::{self as ty, Ty, strip_readonly};

/// Whether copy-on-write is on for this compile. On by default; `COIL_COW=0`
/// turns it off.
pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| !matches!(std::env::var("COIL_COW").as_deref(), Ok("0" | "off")))
}

/// Whether a value of `ty` may be a class instance or array that a write
/// can change: anything but a scalar, a string, a tuple, an enum, a
/// function, a record, an opaque handle, a `readonly class` or a class
/// with a `drop`. A type parameter counts, since it may stand for one.
pub(crate) fn cow_ty(checker: &Checker, ty: Option<&Ty>) -> bool {
    let Some(ty) = ty else { return false };
    let ty = crate::typechecking::subst::apply_ty_prune(checker.subst(), ty);
    let name = match strip_readonly(&ty) {
        // A type parameter (#2161): its ops reach the runtime only where the
        // value is an object word. A per-type clone at a scalar type holds
        // the scalar itself, which MIR passes through with no op; a shared
        // body holds every `T` boxed, and the runtime shares or copies only class
        // instances and arrays.
        Ty::Var(_) => return true,
        Ty::Con(n) => n.as_str(),
        Ty::App(head, _) => match head.as_ref() {
            Ty::Con(n) => n.as_str(),
            _ => return false,
        },
        Ty::Array { .. } | Ty::Existential { .. } => return true,
        _ => return false,
    };
    if super::escape::no_heap(Some(&Ty::Con(name.to_string()))) || matches!(name, "string" | "Option" | "Result") {
        return false;
    }
    // An opaque handle (`Stream`, `Thread`, …) has no fields a write could change (#2155).
    if matches!(name, "Range" | "RangeInclusive") || crate::typechecking::ty::is_opaque_handle(name) {
        return false;
    }
    if name == common::BUILTIN_VEC_TYPE {
        return true;
    }
    if checker.has_impl(name) {
        if !checker.is_class(name) {
            return false;
        }
        let key = checker.resolve_class_key(name).unwrap_or_else(|| name.to_string());
        // A view is the borrow it holds: never shared or copied, and it
        // cannot outlive the place it borrows (E0151).
        return !checker.is_readonly_class(&key) && !checker.is_shared_class(&key) && !checker.is_view_class(&key) && !checker.class_has_drop(name);
    }
    checker.enum_variants(name).is_none()
}

/// Whether `ty` is a bare type parameter: a write through it copies only
/// in the instances where it stands for a class or array, so its E0141 /
/// E0142 warnings would be guesses.
fn open_param(checker: &Checker, ty: Option<&Ty>) -> bool {
    ty.is_some_and(|t| matches!(strip_readonly(&crate::typechecking::subst::apply_ty_prune(checker.subst(), t)), Ty::Var(_)))
}

/// What a call does with each argument (bit `i` for argument `i`).
#[derive(Debug, Clone, Copy, Default)]
struct ArgUse {
    /// The callee may write through the argument.
    written: u64,
    /// The callee may keep the argument past the call.
    kept: u64,
    /// The result may be the argument itself (#2206): the caller shares it
    /// unless the argument is moved into the call.
    returned: u64,
    /// Arguments that reach code not known here: code that shares what it
    /// stores (so `kept` is clear), but a caller's fresh object passed
    /// there may be kept all the same.
    opaque: u64,
    /// The result may be an object held inside the argument, a field or
    /// element the callee returned without sharing (#2206): the caller
    /// shares the result unless it only reads it.
    inner: u64,
}

/// A write through a place, before node `at` runs.
#[derive(Debug, Clone, Copy)]
struct Write {
    at: HirId,
    path: HirId,
}

struct Facts<'b> {
    body: &'b HirBody,
    order: Vec<u32>,
    /// One past the largest order in each node's subtree.
    end: Vec<u32>,
    parent: HashMap<HirId, HirId>,
    calls: HashMap<HirId, ArgUse>,
    /// Calls whose result holds only objects the call made, which nothing
    /// else reaches.
    fresh_calls: HashSet<HirId>,
    writes: Vec<Write>,
    /// The roots of the paths written through.
    written_roots: HashSet<LocalId>,
    /// Each local's reads (not as an assignment's place).
    reads: HashMap<LocalId, Vec<HirId>>,
    /// Each local's assigned values: `let` inits and assignments.
    inits: HashMap<LocalId, Vec<HirId>>,
    /// Pattern bindings: local, the scrutinee / iterable it comes from,
    /// and the node that binds it (`Match` arm body, `ForIn`, `LetPat`).
    bound: HashMap<LocalId, (HirId, HirId)>,
    /// Locals some lambda or `defer` captures.
    captured: HashSet<LocalId>,
    /// Locals a `defer` captures: its body may not assign them, so a write
    /// through one is made unique below the local only.
    defer_captured: HashSet<LocalId>,
    /// The outer locals each lambda or `defer` node captures.
    capture_lists: HashMap<HirId, Vec<LocalId>>,
    /// Outer locals a lambda captures as `use (mut x)`, by lambda.
    borrow_captures: HashSet<(HirId, LocalId)>,
    /// Of those, the ones the closure's body keeps, returns or hands to
    /// code not known here: a holder of the object that may outlive a call.
    loose_captures: HashSet<(HirId, LocalId)>,
    /// Parameters the body assigns.
    reassigned: HashSet<LocalId>,
    /// Assignments to a `use (mut x)` capture (#2072).
    capture_assigns: Vec<HirId>,
    /// Places a `new View(…)` borrows into a `mut` field: never shared.
    borrow_args: HashSet<HirId>,
    /// `for` loops that may keep what they walk past the loop
    /// ([`walk_keeps`]): the iterable is stored, as a kept argument is.
    kept_walks: HashSet<HirId>,
    /// Calls that may write a `static`: not known here, or a summary
    /// with `statics`.
    static_calls: HashSet<HirId>,
}

/// What a call in a body may do: [`CallLookup`] for the body given.
pub(super) type BodyLookup<'c> = dyn Fn(&HirBody, &Callee, &[HirId]) -> Option<Summary> + 'c;

/// The body with its copy-on-write ops, when it needs any, and warnings
/// for writes that copy or are dropped.
pub(super) fn rewrite(checker: &Checker, module: &HirModule, body: &HirBody, lookup: &BodyLookup<'_>, mutates: u64) -> (Option<HirBody>, Vec<Message>) {
    if body.root.is_none() {
        return (None, Vec::new());
    }
    let analyse = |b: &HirBody| {
        let bodies = lookup;
        let lookup = |callee: &Callee, args: &[HirId]| lookup(b, callee, args);
        let facts = Facts::new(checker, module, b, &lookup, bodies, b.root.expect("a body with a root"));
        let plan = Plan::new(checker, b, &facts, mutates);
        let next = view::round(checker, b, &facts, &plan, &lookup);
        (plan, next)
    };
    let (mut plan, mut next) = analyse(body);
    // Loop views ([`view`]): a view stands only if the rewrite of the
    // viewed body draws no diagnostic the body did not and shares no view.
    let mut viewed: Option<HirBody> = None;
    let notes = plan.notes.len();
    for _ in 0..view::ROUNDS {
        let Some(b) = next.take() else { break };
        let (p, n) = analyse(&b);
        if p.notes.len() != notes || view::shares_view(&p, &b, body.locals.len()) {
            break;
        }
        (plan, next, viewed) = (p, n, Some(b));
    }
    let notes = std::mem::take(&mut plan.notes);
    if plan.is_empty() {
        return (viewed, notes);
    }
    let mut out = viewed.unwrap_or_else(|| body.clone());
    plan.apply(&mut out, checker);
    (Some(out), notes)
}

#[path = "cow_view.rs"]
mod view;

/// What [`closure_life`] finds: the first holder, the span, every holder.
type ClosureLife = (Option<LocalId>, (u32, u32), HashSet<LocalId>);

/// How long the closure `lambda` makes lives in `body`: the local first
/// holding it, and the evaluation-order span from the lambda to the last
/// read of any local it reaches (`let f = …`, `let g = f`, another closure
/// capturing one). `Err` with what the closure does when it outlives the
/// body: it is returned, stored in a field, element or value, or passed to
/// a call that may keep it. Third: the locals that hold it.
fn closure_life(body: &HirBody, f: &Facts<'_>, lambda: HirId) -> Result<ClosureLife, &'static str> {
    let start = f.order(lambda);
    let mut end = f.end[lambda.0 as usize].saturating_sub(1).max(start);
    let mut first: Option<LocalId> = None;
    let mut holders: HashSet<LocalId> = HashSet::new();
    let results: HashSet<HirId> = body.root.map(|r| super::escape::result_exprs(body, r)).unwrap_or_default().into_iter().collect();
    let mut work = vec![lambda];
    while let Some(v) = work.pop() {
        // The node the value reaches: through block tails, branches, arms
        // and named arguments.
        let mut at = v;
        let parent = loop {
            let Some(&p) = f.parent.get(&at) else { break None };
            match &body.expr(p).kind {
                HirKind::Block { tail: Some(t), .. } if *t == at => at = p,
                HirKind::If { cond, .. } if *cond != at => at = p,
                HirKind::Match { scrutinee, .. } if *scrutinee != at => at = p,
                HirKind::Named { .. } => at = p,
                _ => break Some(p),
            }
        };
        if results.contains(&at) && body.ret.is_some() {
            return Err("is returned");
        }
        let Some(p) = parent else { continue };
        let mut hold = |l: LocalId, from: HirId, work: &mut Vec<HirId>| {
            if !holders.insert(l) {
                return;
            }
            first.get_or_insert(l);
            end = end.max(f.live_span(l, from).1);
            work.extend(f.reads.get(&l).into_iter().flatten().copied());
            // A closure or `defer` capturing the holder holds it too.
            for (&c, outer) in &f.capture_lists {
                if outer.contains(&l) {
                    match body.expr(c).kind {
                        HirKind::Lambda { .. } => work.push(c),
                        _ => end = u32::MAX - 1,
                    }
                }
            }
        };
        match &body.expr(p).kind {
            HirKind::Let { local, init: Some(i) } if *i == at => hold(*local, p, &mut work),
            HirKind::Assign { place, value } if *value == at => match body.expr(*place).kind {
                HirKind::Local(l) => hold(l, p, &mut work),
                _ => return Err("is stored"),
            },
            HirKind::Return(_) | HirKind::Yield { .. } | HirKind::Resume { .. } => return Err("is returned"),
            HirKind::Append { .. } | HirKind::Make { .. } => return Err("is stored"),
            HirKind::Call { callee, args } => {
                if matches!(callee, Callee::Value(c) if *c == at) {
                    continue;
                }
                let Some(j) = args.iter().position(|&a| a == at) else { continue };
                let u = f.calls.get(&p).copied().unwrap_or_default();
                let bit = if j < 64 { 1u64 << j } else { u64::MAX };
                if (u.kept | u.opaque) & bit != 0 {
                    return Err("is passed to a call that may keep it");
                }
                end = end.max(f.end[p.0 as usize].saturating_sub(1));
                if u.returned & bit != 0 {
                    work.push(p);
                }
            }
            _ => {}
        }
    }
    Ok((first, (start, end), holders))
}

/// How a place reads in a message: `a`, `a.b`, `a[…]`.
fn place_text(body: &HirBody, v: HirId) -> Option<String> {
    match &body.expr(v).kind {
        HirKind::Local(l) => {
            let name = &body.local(*l).name;
            (!name.starts_with('$') && body.local(*l).kind != LocalKind::Temp).then(|| name.clone())
        }
        HirKind::Field { base, name } => Some(format!("{}.{name}", place_text(body, *base)?)),
        HirKind::Index { base, .. } => Some(format!("{}[…]", place_text(body, *base)?)),
        HirKind::Global { name, .. } => Some(name.rsplit("::").next().unwrap_or(name).to_string()),
        HirKind::Builtin { op: Builtin::Cow { .. }, args } if args.len() == 1 => place_text(body, args[0]),
        _ => None,
    }
}

impl<'b> Facts<'b> {
    fn new(checker: &Checker, module: &HirModule, body: &'b HirBody, lookup: &CallLookup<'_>, bodies: &BodyLookup<'_>, root: HirId) -> Self {
        let n = body.exprs.len();
        let mut f = Facts {
            body,
            order: vec![u32::MAX; n],
            end: vec![0; n],
            parent: HashMap::new(),
            calls: HashMap::new(),
            fresh_calls: HashSet::new(),
            writes: Vec::new(),
            written_roots: HashSet::new(),
            reads: HashMap::new(),
            inits: HashMap::new(),
            bound: HashMap::new(),
            captured: HashSet::new(),
            capture_lists: HashMap::new(),
            borrow_captures: HashSet::new(),
            loose_captures: HashSet::new(),
            defer_captured: HashSet::new(),
            reassigned: HashSet::new(),
            capture_assigns: Vec::new(),
            borrow_args: HashSet::new(),
            kept_walks: HashSet::new(),
            static_calls: HashSet::new(),
        };
        // Evaluation order: children in the order `children` lists them.
        let mut next = 0u32;
        let mut stack = vec![(root, false)];
        while let Some((id, done)) = stack.pop() {
            if done {
                f.end[id.0 as usize] = next;
                continue;
            }
            f.order[id.0 as usize] = next;
            next += 1;
            stack.push((id, true));
            for k in body.children(id).rev() {
                f.parent.insert(k, id);
                stack.push((k, false));
            }
        }
        for i in 0..n {
            let id = HirId(i as u32);
            if f.order[i] == u32::MAX {
                continue;
            }
            f.note(checker, module, lookup, id);
        }
        for w in &f.writes {
            if let Some(HirKind::Local(l)) = f.root_of(w.path).map(|r| &body.expr(r).kind) {
                f.written_roots.insert(*l);
            }
        }
        for &(lambda, o) in &f.borrow_captures {
            let HirKind::Lambda { body: inner } = body.expr(lambda).kind else { continue };
            let inner = &module.bodies[inner];
            let Some(&(_, i)) = inner.captures.iter().find(|(x, _)| *x == o) else { continue };
            let (kept, returned, opaque, _) = keeps_of(module, inner, &|c: &Callee, a: &[HirId]| bodies(inner, c, a), &[i], Some(checker));
            if (kept | returned | opaque) & 1 != 0 {
                f.loose_captures.insert((lambda, o));
            }
        }
        f
    }

    fn note(&mut self, checker: &Checker, module: &HirModule, lookup: &CallLookup<'_>, id: HirId) {
        let body = self.body;
        match &body.expr(id).kind {
            HirKind::Local(l) => {
                let is_place = matches!(self.parent.get(&id).map(|p| &body.expr(*p).kind), Some(HirKind::Assign { place, .. }) if *place == id);
                if !is_place {
                    self.reads.entry(*l).or_default().push(id);
                }
            }
            HirKind::Let { local, init: Some(v) } => self.inits.entry(*local).or_default().push(*v),
            HirKind::Assign { place, value } => match &body.expr(*place).kind {
                HirKind::Local(l) => {
                    self.inits.entry(*l).or_default().push(*value);
                    if body.local(*l).kind == LocalKind::Param {
                        self.reassigned.insert(*l);
                    }
                    if body.local(*l).kind == LocalKind::Capture && body.local(*l).borrow {
                        self.capture_assigns.push(id);
                    }
                }
                HirKind::Field { base, .. } => self.writes.push(Write { at: id, path: *base }),
                HirKind::Index { base, kind: IndexKind::Array | IndexKind::Other, .. } => {
                    self.writes.push(Write { at: id, path: *base })
                }
                _ => {}
            },
            HirKind::Append { base, .. } => self.writes.push(Write { at: id, path: *base }),
            // `new View(mut place, …)`: the view writes the place's object
            // in place, so it is made unique here, as for a write.
            HirKind::Make { .. } => {
                for a in crate::hir::view_lint::borrow_args(checker, body, id) {
                    self.writes.push(Write { at: id, path: a });
                    self.borrow_args.insert(a);
                }
            }
            HirKind::Call { callee, args } => {
                let mut used = arg_use(body, lookup, callee, args);
                // A call that returns a view borrows its receiver (E0151
                // lets only a method of the borrowed object return one): the
                // receiver is written through the view, never shared.
                if crate::hir::view_lint::is_view_ty(checker, body.expr(id).ty.as_ref())
                    && args.first().is_some_and(|&r| cow_ty(checker, body.expr(r).ty.as_ref()))
                {
                    used.kept &= !1;
                    used.opaque &= !1;
                    used.written |= 1;
                }
                // A call that yields a function may be a partial application
                // (`g(q)`, `q.m(1)`), whose closure keeps the arguments given:
                // each has a second name from here on (#2788), as `escape`
                // counts it.
                if matches!(body.expr(id).ty.as_ref().map(strip_readonly), Some(Ty::Fun(..))) {
                    used.kept |= if args.len() >= 64 { u64::MAX } else { (1u64 << args.len()) - 1 };
                }
                if std::env::var("COIL_COW_TRACE").is_ok() {
                    eprintln!("cow:   {callee:?} written={:b} kept={:b} summary={:?}", used.written, used.kept, lookup(callee, args).map(|s| (s.mutates, s.keeps, s.returns)));
                }
                for (i, &a) in args.iter().enumerate().take(64) {
                    let a = named_value(body, a);
                    if used.written & (1 << i) != 0 && cow_ty(checker, body.expr(a).ty.as_ref()) {
                        self.writes.push(Write { at: id, path: a });
                    }
                }
                let summary = lookup(callee, args);
                if summary.as_ref().is_some_and(|s| !s.shared_ret) {
                    self.fresh_calls.insert(id);
                }
                let structural = matches!(callee, Callee::Method { .. }) && args.first().is_some_and(|r| is_vec(body.expr(*r).ty.as_ref()));
                let host = matches!(callee, Callee::Named { name, .. } if host_short(name).is_some());
                if summary.as_ref().map_or(!structural && !host, |s| s.statics) {
                    self.static_calls.insert(id);
                }
                self.calls.insert(id, used);
            }
            HirKind::Match { scrutinee, arms } => {
                for arm in arms {
                    let mut ls = Vec::new();
                    arm.pat.for_each_bind(&mut |l| ls.push(l));
                    for l in ls {
                        self.bound.insert(l, (*scrutinee, arm.body));
                    }
                }
            }
            HirKind::ForIn { pat, iterable, kind, .. } => {
                if walk_keeps(module, body, lookup, id) {
                    self.kept_walks.insert(id);
                }
                if matches!(kind, Some(ForInKind::Array)) {
                    let mut ls = Vec::new();
                    pat.for_each_bind(&mut |l| ls.push(l));
                    for l in ls {
                        self.bound.insert(l, (*iterable, id));
                    }
                }
            }
            HirKind::LetPat { pat, init } => {
                let mut ls = Vec::new();
                pat.for_each_bind(&mut |l| ls.push(l));
                for l in ls {
                    self.bound.insert(l, (*init, id));
                }
            }
            HirKind::Lambda { body: inner } => {
                let inner_body = &module.bodies[*inner];
                let outer: Vec<LocalId> = inner_body.captures.iter().map(|(o, _)| *o).collect();
                for (o, i) in &inner_body.captures {
                    if inner_body.local(*i).borrow {
                        self.borrow_captures.insert((id, *o));
                    }
                }
                self.captured.extend(outer.iter().copied());
                self.capture_lists.insert(id, outer);
            }
            HirKind::Defer { captures, .. } => {
                let outer: Vec<LocalId> = captures.iter().flatten().copied().collect();
                self.defer_captured.extend(outer.iter().copied());
                self.captured.extend(outer.iter().copied());
                self.capture_lists.insert(id, outer);
            }
            _ => {}
        }
    }

    /// The block and the statement (or tail) of it that holds `id`.
    /// Stops early at a branch, a loop body, an arm or a `&&` / `||`
    /// right side: `(id, id)` where the node itself is the one to run
    /// first, wrapped in a block.
    fn statement_of(&self, id: HirId) -> (HirId, HirId) {
        let mut at = id;
        while let Some(&p) = self.parent.get(&at) {
            match &self.body.expr(p).kind {
                HirKind::Block { .. } => return (p, at),
                HirKind::If { cond, .. } if *cond != at => return (at, at),
                HirKind::Match { scrutinee, .. } if *scrutinee != at => return (at, at),
                HirKind::Logic { rhs, .. } if *rhs == at => return (at, at),
                HirKind::ForIn { body, .. } | HirKind::Loop { body } | HirKind::Defer { body, .. } if *body == at => return (at, at),
                _ => {}
            }
            at = p;
        }
        (at, at)
    }

    fn order(&self, id: HirId) -> u32 {
        self.order[id.0 as usize]
    }

    /// The root of a place path: below fields, array elements and `Cow`
    /// ops. `None` for a path through anything else.
    fn root_of(&self, mut id: HirId) -> Option<HirId> {
        loop {
            match &self.body.expr(id).kind {
                HirKind::Field { base, .. } | HirKind::Index { base, kind: IndexKind::Array, .. } => id = *base,
                HirKind::Builtin { op: Builtin::Cow { .. }, args } if args.len() == 1 => id = args[0],
                _ => return Some(id),
            }
        }
    }

    /// The innermost loop around `id` (a `Loop` or `ForIn` body).
    fn loops_around(&self, id: HirId) -> Vec<HirId> {
        let mut out = Vec::new();
        let mut at = id;
        while let Some(&p) = self.parent.get(&at) {
            match &self.body.expr(p).kind {
                HirKind::Loop { .. } => out.push(p),
                HirKind::ForIn { body: b, .. } if *b == at => out.push(p),
                _ => {}
            }
            at = p;
        }
        out
    }

    fn contains(&self, outer: HirId, inner: HirId) -> bool {
        let (o, i) = (self.order(outer), self.order(inner));
        o <= i && i < self.end[outer.0 as usize]
    }

    /// The evaluation-order span over which a value bound at `from` may
    /// still be read through `l`: to its last read, widened to the whole of
    /// any loop that reads it but does not hold the binding.
    fn live_span(&self, l: LocalId, from: HirId) -> (u32, u32) {
        let start = self.order(from);
        let mut end = start;
        for &r in self.reads.get(&l).map(Vec::as_slice).unwrap_or(&[]) {
            end = end.max(self.order(r));
            for lp in self.loops_around(r) {
                if !self.contains(lp, from) {
                    end = end.max(self.end[lp.0 as usize]);
                }
            }
        }
        (start, end)
    }

    /// Whether `read` (of `l`) is the last read of a local nothing else
    /// names: no read follows, and no loop around it repeats it.
    fn last_read(&self, l: LocalId, read: HirId) -> bool {
        let at = self.order(read);
        let reads = self.reads.get(&l).map(Vec::as_slice).unwrap_or(&[]);
        if reads.iter().any(|&r| self.order(r) > at) {
            return false;
        }
        let decl = self.inits.get(&l).and_then(|v| v.first()).copied();
        self.loops_around(read).iter().all(|&lp| decl.is_some_and(|d| self.contains(lp, d)))
    }
}

/// The argument `a` of a call, through `name: value`.
fn named_value(body: &HirBody, a: HirId) -> HirId {
    match &body.expr(a).kind {
        HirKind::Named { value, .. } => *value,
        _ => a,
    }
}

/// Bit `i`: `body` may keep the object passed as its parameter `i` past
/// the call: it stores the parameter (or a local that holds it) into a
/// field, an element, a new object, a callee that keeps it, a closure or
/// its result. What it stores from inside the object it shares itself.
/// Second: the parameters that may be its result, as themselves (#2206).
/// Third: the parameters it hands, as themselves, to code not known
/// here, which may keep them.
pub(super) fn keeps(module: &HirModule, body: &HirBody, lookup: &CallLookup<'_>) -> (u64, u64, u64, u64) {
    keeps_of(module, body, lookup, &body.params, None)
}

/// [`keeps`] for the locals `seeds` (bit `i` for `seeds[i]`) in place of
/// the parameters: a closure's captures. With `views`, a call that returns
/// a view does not keep its receiver.
fn keeps_of(module: &HirModule, body: &HirBody, lookup: &CallLookup<'_>, seeds: &[LocalId], views: Option<&Checker>) -> (u64, u64, u64, u64) {
    if body.is_coro {
        return (u64::MAX, u64::MAX, u64::MAX, 0);
    }
    let Some(root) = body.root else { return (0, 0, 0, 0) };
    // What each local may hold of the parameters, through `let x = p`.
    let mut holds: HashMap<LocalId, u64> = HashMap::new();
    for (i, &p) in seeds.iter().enumerate().take(64) {
        holds.insert(p, 1 << i);
    }
    let mut copies: Vec<(LocalId, HirId)> = Vec::new();
    for e in &body.exprs {
        match &e.kind {
            HirKind::Let { local, init: Some(v) } => copies.push((*local, *v)),
            HirKind::Assign { place, value } => {
                if let HirKind::Local(l) = body.expr(*place).kind {
                    copies.push((l, *value));
                }
            }
            _ => {}
        }
    }
    let of = |holds: &HashMap<LocalId, u64>, v: HirId| -> u64 {
        let mut bits = 0;
        let mut work = vec![v];
        while let Some(at) = work.pop() {
            match &body.expr(at).kind {
                HirKind::Block { tail: Some(t), .. } => work.push(*t),
                HirKind::If { then, els, .. } => work.extend(std::iter::once(*then).chain(*els)),
                HirKind::Match { arms, .. } => work.extend(arms.iter().map(|a| a.body)),
                HirKind::Named { value, .. } => work.push(*value),
                HirKind::Builtin { op: Builtin::Cow { .. }, args } => work.extend(args.iter().copied()),
                HirKind::Local(l) => bits |= holds.get(l).copied().unwrap_or(0),
                // The result may be an argument the callee hands back.
                HirKind::Call { callee, args } => {
                    let used = arg_use(body, lookup, callee, args);
                    for (j, &a) in args.iter().enumerate().take(64) {
                        if used.returned & (1 << j) != 0 {
                            work.push(named_value(body, a));
                        }
                    }
                }
                _ => {}
            }
        }
        bits
    };
    loop {
        let mut changed = false;
        for &(l, v) in &copies {
            let b = of(&holds, v);
            let h = holds.entry(l).or_default();
            if *h | b != *h {
                *h |= b;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut kept = 0u64;
    let mut returned = 0u64;
    let mut opaque = 0u64;
    let mut inner = 0u64;
    for (i, e) in body.exprs.iter().enumerate() {
        let id = HirId(i as u32);
        match &e.kind {
            HirKind::Assign { place, value } if !matches!(body.expr(*place).kind, HirKind::Local(_)) => kept |= of(&holds, *value),
            HirKind::Append { value, .. } => kept |= of(&holds, *value),
            HirKind::Make { kind, args } if !matches!(kind, MakeKind::Range { .. }) => {
                for &a in args {
                    kept |= of(&holds, a);
                }
            }
            HirKind::Call { callee, args } => {
                let mut used = arg_use(body, lookup, callee, args);
                // A call that returns a view borrows its receiver, as
                // `Facts::note` reads it: the view cannot outlive it (E0151).
                if let Some(checker) = views
                    && crate::hir::view_lint::is_view_ty(checker, body.expr(id).ty.as_ref())
                    && args.first().is_some_and(|&r| cow_ty(checker, body.expr(r).ty.as_ref()))
                {
                    used.kept &= !1;
                    used.opaque &= !1;
                }
                if std::env::var("COIL_COW_TRACE").is_ok() && used.kept != 0 {
                    eprintln!("keeps   {callee:?} kept={:b}", used.kept);
                }
                for (j, &a) in args.iter().enumerate().take(64) {
                    if used.kept & (1 << j) != 0 {
                        kept |= of(&holds, a);
                    }
                    if used.opaque & (1 << j) != 0 {
                        opaque |= of(&holds, a);
                    }
                }
            }
            HirKind::Return(Some(v)) => {
                returned |= of(&holds, *v);
                inner |= inner_params(body, *v);
            }
            HirKind::Yield { value: v, .. } | HirKind::Resume { value: Some(v), .. } => kept |= of(&holds, *v),
            // A walk whose iterator dies with the loop keeps nothing.
            HirKind::ForIn { iterable, .. } if walk_keeps(module, body, lookup, id) => kept |= of(&holds, *iterable),
            HirKind::Lambda { body: inner } => {
                for (o, _) in &module.bodies[*inner].captures {
                    kept |= holds.get(o).copied().unwrap_or(0);
                }
            }
            // A defer runs before the frame is left, so its captures are
            // not kept past the call.
            _ => {}
        }
        let _ = id;
    }
    if body.ret.is_some() {
        for v in super::escape::result_exprs(body, root) {
            returned |= of(&holds, v);
            inner |= inner_params(body, v);
        }
    }
    (kept, returned, opaque, inner)
}

/// The parameter whose field or element `v` reads (through `p.f`,
/// `p.xs[i]`, …), if any: what a `return v` hands back unshared (#2206).
fn inner_param(body: &HirBody, v: HirId) -> Option<usize> {
    let mut at = v;
    let mut steps = 0;
    loop {
        match &body.expr(at).kind {
            HirKind::Field { base, .. } | HirKind::Index { base, kind: IndexKind::Array, .. } => {
                steps += 1;
                at = *base;
            }
            HirKind::Builtin { op: Builtin::Cow { .. }, args } if args.len() == 1 => at = args[0],
            HirKind::Local(l) if steps > 0 => {
                let local = body.local(*l);
                if local.kind != LocalKind::Param || local.borrow {
                    return None;
                }
                return body.params.iter().position(|p| p == l).filter(|&i| i < 64);
            }
            _ => return None,
        }
    }
}

/// The parameters whose fields or elements a returned `v` may be, through
/// block tails and branches.
fn inner_params(body: &HirBody, v: HirId) -> u64 {
    let mut bits = 0;
    let mut work = vec![v];
    while let Some(at) = work.pop() {
        match &body.expr(at).kind {
            HirKind::Block { tail: Some(t), .. } => work.push(*t),
            HirKind::If { then, els, .. } => work.extend(std::iter::once(*then).chain(*els)),
            HirKind::Match { arms, .. } => work.extend(arms.iter().map(|a| a.body)),
            _ => {
                if let Some(i) = inner_param(body, at) {
                    bits |= 1 << i;
                }
            }
        }
    }
    bits
}

/// What the callee of `callee(args)` does with each argument.
fn arg_use(body: &HirBody, lookup: &CallLookup<'_>, callee: &Callee, args: &[HirId]) -> ArgUse {
    let all = if args.len() >= 64 { u64::MAX } else { (1u64 << args.len()) - 1 };
    let unknown = ArgUse { written: all, kept: 0, returned: all, opaque: all, inner: all };
    let positional = args.iter().all(|a| !matches!(body.expr(*a).kind, HirKind::Named { .. } | HirKind::Spread(_)));
    // A fixed array's methods are structural reads, like a `Vec`'s `len`.
    let recv_vec = matches!(callee, Callee::Method { .. })
        && args.first().is_some_and(|r| {
            let ty = body.expr(*r).ty.as_ref();
            is_vec(ty) || matches!(ty.map(strip_readonly), Some(Ty::Array { .. }))
        });
    if let Callee::Method { name } = callee
        && recv_vec
    {
        let mut used = ArgUse::default();
        if VEC_MUTATORS.contains(&name.as_str()) {
            used.written |= 1;
        }
        match name.as_str() {
            "push" => used.kept |= 1 << 1,
            "insert" => used.kept |= 1 << 2,
            _ => {}
        }
        if used.written != 0 || used.kept != 0 {
            return used;
        }
    }
    let Some(s) = lookup(callee, args) else {
        // Structural reads of a `Vec` (`len`, `capacity`) keep nothing.
        if recv_vec {
            return ArgUse::default();
        }
        if let Callee::Named { name, .. } = callee
            && let Some(short) = host_short(name)
        {
            return host_use(body, short, args, all);
        }
        return unknown;
    };
    if !positional {
        let any = |bits: u64| if bits != 0 { all } else { 0 };
        return ArgUse {
            written: any(s.mutates),
            kept: any(s.keeps),
            returned: any(s.returns_arg | s.opaque_args),
            opaque: any(s.opaque_args),
            inner: any(s.returns_inner | s.opaque_args),
        };
    }
    from_summary(&s)
}

/// The bare name of a host native callee, if `name` is one.
fn host_short(name: &str) -> Option<&str> {
    let short = name.rsplit("::").next().unwrap_or(name);
    common::host_native_id(short).map(|_| short)
}

/// Host natives write only buffers they are handed, and keep arguments
/// only when they hand them to another thread, task or the collector.
fn host_use(body: &HirBody, short: &str, args: &[HirId], all: u64) -> ArgUse {
    let mut used = ArgUse::default();
    for (i, a) in args.iter().enumerate().take(64) {
        let ty = body.expr(*a).ty.as_ref();
        if is_vec(ty) || matches!(ty, Some(Ty::Array { .. })) {
            used.written |= 1 << i;
        }
    }
    let keeps = ["thread_", "task_", "gc_", "await", "drive", "ffi", "black"];
    if keeps.iter().any(|k| short.starts_with(k)) {
        used.kept = all;
        used.written = all;
    }
    used
}

/// A callee shares what it returns from a place (its `return` is a
/// store), so returning part of an argument does not keep the argument.
/// A returned parameter itself is the caller's to share (#2206); code not
/// known here may hand back what it is given.
fn from_summary(s: &Summary) -> ArgUse {
    ArgUse {
        written: s.mutates,
        kept: s.keeps,
        returned: s.returns_arg | s.opaque_args,
        opaque: s.opaque_args,
        inner: s.returns_inner | s.opaque_args,
    }
}

/// A borrow as [`Plan::hold_borrowed`] reads it.
struct Held {
    root: LocalId,
    path: Vec<Option<String>>,
    /// The evaluation-order span over which it lives.
    span: (u32, u32),
    /// The node that makes it: nothing inside it is in the borrow's life.
    within: Option<HirId>,
    /// A `use (mut root)` capture whose closure body may let a holder of
    /// the object outlive a call ([`Facts::loose_captures`]).
    loose: bool,
}

/// Where the rewrite puts ops.
#[derive(Default)]
struct Plan {
    /// Warnings about copies and dropped writes.
    notes: Vec<Message>,
    /// Values to share: (node, eager copy).
    share: Vec<(HirId, bool)>,
    /// Places stored where they outlive the read, shared or moved.
    stored: Vec<HirId>,
    /// Locals to share as a statement right after the node that binds
    /// them (pattern bindings) or before a node (captures).
    share_after: Vec<(HirId, LocalId)>,
    share_before: Vec<((HirId, HirId), LocalId)>,
    /// Locals to make unique as a statement before a node (borrowed
    /// captures).
    unique_before: Vec<((HirId, HirId), LocalId)>,
    /// Write paths to make unique before their node.
    unique: Vec<(Write, (HirId, HirId))>,
    /// Borrows (`let w = mut a.f`, `use (mut a)`, views, `for mut`) by
    /// their root, place steps, live span and the node that makes them.
    borrows: Vec<Held>,
    /// How many objects, from the root down, a write path leaves as they
    /// are (#2816): the borrowed object and the ones holding it, while a
    /// borrow of them lives. Keyed by the write's node and path.
    held: HashMap<(HirId, HirId), usize>,
    /// Locals never shared and only ever given fresh objects.
    fresh_locals: HashSet<LocalId>,
    /// Writes (node, path) in the body of a `for` over an iterator whose
    /// iterable has the write's root: the iterator holds that object, so the
    /// write keeps its unique on a fresh local too (#2819).
    walked: HashSet<(HirId, HirId)>,
    /// Parameters written in place for the caller.
    inout: HashSet<LocalId>,
    /// Locals a `defer` captures.
    defer_captured: HashSet<LocalId>,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.share.is_empty() && self.share_after.is_empty() && self.share_before.is_empty() && self.unique.is_empty() && self.unique_before.is_empty()
    }

    fn new(checker: &Checker, body: &HirBody, f: &Facts<'_>, mutates: u64) -> Self {
        let mut plan = Plan::default();
        let cow = |id: HirId| cow_ty(checker, body.expr(id).ty.as_ref());
        let cow_local = |l: LocalId| cow_ty(checker, body.local(l).ty.as_ref());
        // Parameters written in place for the caller: never copied as a
        // whole here, so sharing one copies it eagerly instead.
        // A coroutine's frame outlives the call that starts it (#2662): its
        // writes run on `resume`, after the caller has its value back, so
        // a parameter is its own value, made unique where it is written.
        let inout: HashSet<LocalId> = body
            .params
            .iter()
            .enumerate()
            .filter(|(i, l)| !body.is_coro && *i < 64 && mutates & (1 << i) != 0 && !f.reassigned.contains(l) && !f.captured.contains(l))
            .map(|(_, l)| *l)
            .chain(body.locals.iter().enumerate().filter(|(_, x)| x.borrow).map(|(i, _)| LocalId(i as u32)))
            .collect();
        let borrow = |l: LocalId| body.local(l).borrow;

        // Bindings from a place: shared unless the binding is a read-only
        // borrow (nothing writes through it, and nothing writes through its
        // source's root while it lives).
        let mut borrowed: HashSet<LocalId> = HashSet::new();
        // Bindings shared with the place they were read from.
        let mut copied_from: HashMap<LocalId, HirId> = HashMap::new();
        let place_root = |v: HirId| -> Option<HirId> {
            match &body.expr(v).kind {
                HirKind::Local(_) | HirKind::Field { .. } | HirKind::Index { .. } | HirKind::Global { .. } => f.root_of(v),
                _ => None,
            }
        };
        let needs_share = |l: LocalId, src_root: HirId, from: HirId| -> bool {
            if f.written_roots.contains(&l) || f.captured.contains(&l) {
                return true;
            }
            let root_local = match &body.expr(src_root).kind {
                HirKind::Local(s) => *s,
                // A static: any call may write it.
                _ => return true,
            };
            let (start, end) = f.live_span(l, from);
            f.writes.iter().any(|w| {
                let o = f.order(w.at);
                o >= start
                    && o <= end
                    && f.root_of(w.path).is_none_or(|r| match &body.expr(r).kind {
                        HirKind::Local(x) => *x == root_local || aliases(f, body, *x, root_local),
                        _ => true,
                    })
            })
        };
        for (&l, inits) in &f.inits {
            if !cow_local(l) {
                continue;
            }
            // `let c = mut place`: the place's object made unique where it
            // is, and `c` names it.
            if borrow(l) {
                for &v in inits {
                    if place_root(v).is_some() {
                        let at = f.parent.get(&v).copied().unwrap_or(v);
                        plan.unique.push((Write { at, path: v }, f.statement_of(at)));
                    }
                }
                continue;
            }
            for &v in inits {
                let Some(r) = place_root(v) else { continue };
                if let HirKind::Local(m) = body.expr(v).kind
                    && f.last_read(m, v)
                    && owned(f, body, m, &inout)
                {
                    continue;
                }
                let from = f.parent.get(&v).copied().unwrap_or(v);
                if needs_share(l, r, from) {
                    copied_from.insert(l, v);
                    plan.share_value(body, f, v, &inout);
                } else {
                    borrowed.insert(l);
                }
            }
        }
        for (&l, &(src, binder)) in &f.bound {
            if !cow_local(l) {
                continue;
            }
            if borrow(l) {
                if place_root(src).is_some() {
                    let at = f.parent.get(&src).copied().unwrap_or(src);
                    plan.unique.push((Write { at, path: src }, f.statement_of(at)));
                }
                continue;
            }
            let Some(r) = place_root(src) else {
                // From a fresh value (a call's result): the payload was
                // shared where it was stored.
                continue;
            };
            if needs_share(l, r, binder) {
                copied_from.insert(l, src);
                plan.share_after.push((binder, l));
            } else {
                borrowed.insert(l);
            }
        }

        // Stores: the value now has a second name.
        for i in 0..body.exprs.len() {
            let id = HirId(i as u32);
            if f.order[i] == u32::MAX {
                continue;
            }
            match &body.expr(id).kind {
                HirKind::Assign { place, value } if !matches!(body.expr(*place).kind, HirKind::Local(_)) => {
                    plan.store(body, f, *value, &cow, &inout, &borrowed)
                }
                HirKind::Append { value, .. } => plan.store(body, f, *value, &cow, &inout, &borrowed),
                HirKind::Make { kind, args } if !matches!(kind, MakeKind::Range { .. }) => {
                    for &a in args {
                        let v = named_value(body, a);
                        if !f.borrow_args.contains(&v) {
                            plan.store(body, f, v, &cow, &inout, &borrowed);
                        }
                    }
                }
                HirKind::Call { args, .. } => {
                    let u = f.calls.get(&id).copied().unwrap_or_default();
                    let mut copy_result = false;
                    let mut share_result = false;
                    let partial = matches!(body.expr(id).ty.as_ref().map(strip_readonly), Some(Ty::Fun(..)));
                    for (j, &a) in args.iter().enumerate().take(64) {
                        let bit = 1u64 << j;
                        let v = named_value(body, a);
                        if partial && u.written & bit != 0 && cow(v) {
                            // A partial of a function that writes this
                            // parameter writes on each later call: it takes a
                            // copy of its own, as a `use (x)` capture is
                            // (#2788).
                            plan.share.push((v, true));
                        } else if u.kept & bit != 0 {
                            plan.store(body, f, v, &cow, &inout, &borrowed);
                        } else if u.returned & bit != 0
                            && cow(id)
                            && cow(v)
                            && !moved_into_call(f, body, id, v, &inout, &borrowed)
                        {
                            // The result may be this argument while its
                            // place lives on (#2206). One the callee writes
                            // gets the eager copy the callee used to make.
                            // A result that is not a COW value cannot be it.
                            // Opaque code may write the argument as well,
                            // and is assumed to: its result is shared after
                            // the call instead (writes it made are done).
                            // An argument written in place for the caller (a
                            // borrow, an inout parameter) is never tested
                            // before a write, so a result that may be it is
                            // a copy, as `let r = w` copies it (#2818).
                            // Only where the local is written after the call.
                            let in_place = !read_only_result(f, body, id)
                                && matches!(body.expr(v).kind, HirKind::Local(l) if inout.contains(&l) && written_after(f, body, l, id));
                            if u.opaque & bit != 0 && !in_place {
                                share_result = true;
                            } else if u.written & bit != 0 || u.opaque & bit != 0 {
                                copy_result = true;
                            } else {
                                plan.store(body, f, v, &cow, &inout, &borrowed);
                            }
                        }
                    }
                    // The result may be an object inside an argument the
                    // callee handed back unshared: share it unless this
                    // only reads it or that argument is moved in.
                    let inside = args.iter().enumerate().take(64).any(|(j, &a)| {
                        let v = named_value(body, a);
                        u.inner & (1u64 << j) != 0
                            && may_hold_objects(body.expr(v).ty.as_ref())
                            && !(cow(v) && moved_into_call(f, body, id, v, &inout, &borrowed))
                    });
                    if !copy_result && (inside || share_result) && cow(id) && !read_only_result(f, body, id) {
                        plan.share.push((id, false));
                    }
                    if copy_result {
                        plan.share.push((id, true));
                    }
                }
                HirKind::Return(Some(v)) => plan.store_result(body, f, *v, &cow, &inout, &borrowed),
                // A cast between byte arrays is the operand's object (#2823):
                // the result is a second name for it.
                HirKind::Cast { value } if cow(*value) && cow(id) => plan.store(body, f, *value, &cow, &inout, &borrowed),
                HirKind::ForIn { iterable, .. } if f.kept_walks.contains(&id) => plan.store(body, f, *iterable, &cow, &inout, &borrowed),
                HirKind::Yield { value: v, .. } | HirKind::Resume { value: Some(v), .. } => {
                    plan.store(body, f, *v, &cow, &inout, &borrowed)
                }
                HirKind::Lambda { .. } => {
                    for &l in f.capture_lists.get(&id).map(Vec::as_slice).unwrap_or(&[]) {
                        if !cow_local(l) {
                            continue;
                        }
                        if f.borrow_captures.contains(&(id, l)) {
                            // `use (mut l)`: the closure writes `l`'s object.
                            if !inout.contains(&l) {
                                plan.unique_before.push((f.statement_of(id), l));
                            }
                        } else {
                            plan.share_before.push((f.statement_of(id), l));
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(root) = body.root {
            for v in super::escape::result_exprs(body, root) {
                if body.ret.is_some() {
                    plan.store_result(body, f, v, &cow, &inout, &borrowed);
                }
            }
        }

        // Locals only ever given fresh objects and never shared need no
        // unique before a write.
        let shared_locals: HashSet<LocalId> = plan
            .share
            .iter()
            .filter_map(|&(v, _)| match body.expr(v).kind {
                HirKind::Local(l) => Some(l),
                _ => None,
            })
            .chain(plan.share_before.iter().map(|&(_, l)| l))
            .chain(plan.share_after.iter().map(|&(_, l)| l))
            .collect();
        let kept_locals: HashSet<LocalId> = f
            .calls
            .iter()
            .flat_map(|(&c, u)| {
                let HirKind::Call { args, .. } = &body.expr(c).kind else { unreachable!() };
                args.iter()
                    .enumerate()
                    .take(64)
                    .filter(move |(j, _)| (u.opaque | u.kept | u.returned) & (1 << j) != 0)
                    .filter_map(|(_, &a)| match body.expr(named_value(body, a)).kind {
                        HirKind::Local(l) => Some(l),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let borrowed_from: HashSet<LocalId> = borrowed
            .iter()
            .flat_map(|b| f.inits.get(b).into_iter().flatten().chain(f.bound.get(b).map(|(s, _)| s)))
            .filter_map(|&v| f.root_of(v))
            .filter_map(|r| match body.expr(r).kind {
                HirKind::Local(l) => Some(l),
                _ => None,
            })
            .collect();
        for (&l, inits) in &f.inits {
            let local = body.local(l);
            if matches!(local.kind, LocalKind::Param | LocalKind::Capture)
                || f.bound.contains_key(&l)
                || f.captured.contains(&l)
                || shared_locals.contains(&l)
                || kept_locals.contains(&l)
                || borrowed_from.contains(&l)
            {
                continue;
            }
            if inits.iter().all(|&v| fresh_ctor(body, v) || f.fresh_calls.contains(&v)) {
                plan.fresh_locals.insert(l);
            }
        }

        // A `for` over a `Vec` or an array walks the value its iterable had
        // when the loop began (#2827): where the body may write it, the
        // loop takes a share of it at its start, so the first write copies
        // it once, and the body's writes to it keep their unique on a fresh
        // local too. A parameter written in place for the caller is never
        // tested before a write: the loop walks an eager copy of it, over a
        // `Vec` or an array or through an iterator, which would otherwise
        // hold the object the body writes in place (#2826; a walk that keeps
        // its iterable stores it, which copies it already). After
        // `fresh_locals`: past the loop its other holder is dead.
        let mut holding = HashSet::new();
        for i in 0..body.exprs.len() {
            let id = HirId(i as u32);
            if f.order[i] == u32::MAX || !walk_written(body, f, id) {
                continue;
            }
            let HirKind::ForIn { iterable, kind, .. } = &body.expr(id).kind else { continue };
            let iterable = *iterable;
            let eager = matches!(body.expr(iterable).kind, HirKind::Local(l) if inout.contains(&l));
            if !cow(iterable) {
                continue;
            }
            if matches!(kind, Some(ForInKind::Array)) {
                plan.share.push((iterable, eager));
                holding.insert(id);
            } else if eager && !f.kept_walks.contains(&id) {
                plan.share.push((iterable, true));
            }
        }

        // Writes: make each path unique top-down.
        for &w in &f.writes {
            if let Some(HirKind::Local(l)) = f.root_of(w.path).map(|r| &body.expr(r).kind)
                && inout.contains(l)
                && w.path == f.root_of(w.path).unwrap_or(w.path)
            {
                // The parameter itself, written in place for the caller.
                continue;
            }
            if walked_write(body, f, w, &holding) {
                plan.walked.insert((w.at, w.path));
            }
            plan.unique.push((w, f.statement_of(w.at)));
        }
        plan.inout = inout;
        plan.defer_captured = f.defer_captured.clone();
        plan.check_borrows(checker, body, f);
        plan.hold_borrowed(body, f);
        plan.check_kept_views(checker, body, f);
        plan.note_copies(checker, body, f, &copied_from);
        plan
    }

    /// E0151 (#2198): a view handed to a callee that keeps it (stores it,
    /// captures it in a closure it returns, …) outlives its borrow, though
    /// the callee only sees a `T`. A view's own method that returns a view
    /// keeps its receiver by design: that is the call chain E0151 allows.
    fn check_kept_views(&mut self, checker: &Checker, body: &HirBody, f: &Facts<'_>) {
        let mut calls: Vec<(&HirId, &ArgUse)> = f.calls.iter().collect();
        calls.sort_by_key(|(id, _)| id.0);
        for (&id, used) in calls {
            let HirKind::Call { args, .. } = &body.expr(id).kind else {
                continue;
            };
            let returns_view = crate::hir::view_lint::is_view_ty(checker, body.expr(id).ty.as_ref());
            for (i, &a) in args.iter().enumerate().take(64) {
                let v = named_value(body, a);
                if used.kept & (1 << i) == 0 || (i == 0 && returns_view) {
                    continue;
                }
                let Some(view) = crate::hir::view_lint::view_key(checker, body.expr(v).ty.as_ref()) else {
                    continue;
                };
                let short = view.rsplit("::").next().unwrap_or(&view);
                let span = body.expr(v).span;
                let mut m = Message::error(
                    ErrorCode::ViewEscape,
                    format!("this call keeps the `{short}` view past the call, where it would outlive what it borrows"),
                    span.0..span.1,
                );
                m.with_help("a view is the borrow it holds: pass it only to calls that use it and let it go".to_string());
                self.notes.push(m);
            }
        }
    }

    /// E0149 (#2191): a borrow writes its object in place, so while it
    /// lives nothing may give that object a second name (a share of the
    /// borrowed place or of a place holding it) or replace the place (the
    /// borrow would write an orphan). A share of a place below the borrowed
    /// object is fine: the borrow's write makes that child unique first.
    fn check_borrows(&mut self, checker: &Checker, body: &HirBody, f: &Facts<'_>) {
        struct Borrow {
            name: String,
            root: LocalId,
            path: Vec<Option<String>>,
            text: String,
            span: (u32, u32),
            /// The borrowing expression: nothing inside it conflicts.
            within: Option<HirId>,
            /// A `use (mut x)` capture: `name` is the closure.
            capture: bool,
            /// The borrowing local (a `let`, a `for mut` binding, a view),
            /// whose writes reach the borrowed object.
            local: Option<LocalId>,
            /// The locals holding a `use (mut x)` closure: calling one may
            /// write the borrowed object.
            holders: HashSet<LocalId>,
        }
        let mut borrows: Vec<Borrow> = Vec::new();
        let mut add = |name: &str, local: LocalId, src: HirId, span: (u32, u32), within: Option<HirId>| {
            if let (Some((root, path)), Some(text)) = (place_path(body, f, src), place_text(body, src)) {
                borrows.push(Borrow { name: name.to_string(), root, path, text, span, within, capture: false, local: Some(local), holders: HashSet::new() });
            }
        };
        for (&l, inits) in &f.inits {
            let local = body.local(l);
            let view = crate::hir::view_lint::is_view_ty(checker, local.ty.as_ref());
            if !(local.borrow && cow_ty(checker, local.ty.as_ref())) && !view {
                continue;
            }
            for &v in inits {
                let from = f.parent.get(&v).copied().unwrap_or(v);
                let span = f.live_span(l, from);
                if !view {
                    add(&local.name, l, v, span, Some(v));
                    continue;
                }
                // A view borrows the places its `mut` fields were given, or
                // (from a method) the object the method was called on.
                let mut at = v;
                loop {
                    match &body.expr(at).kind {
                        HirKind::Make { .. } => {
                            for a in crate::hir::view_lint::borrow_args(checker, body, at) {
                                add(&local.name, l, a, span, Some(v));
                            }
                            break;
                        }
                        HirKind::Call { args, .. } if !args.is_empty() => {
                            let r = args[0];
                            if crate::hir::view_lint::is_view_ty(checker, body.expr(r).ty.as_ref()) && matches!(body.expr(r).kind, HirKind::Call { .. } | HirKind::Make { .. }) {
                                at = r;
                                continue;
                            }
                            add(&local.name, l, r, span, Some(v));
                            break;
                        }
                        _ => break,
                    }
                }
            }
        }
        for (&l, &(src, binder)) in &f.bound {
            if body.local(l).borrow && cow_ty(checker, body.local(l).ty.as_ref()) {
                add(&body.local(l).name, l, src, f.live_span(l, binder), Some(src));
            }
        }
        // `use (mut a)`: the closure borrows `a`'s object for as long as it
        // lives (#2787), as `let w = mut a` would. A closure that outlives
        // the body (returned, stored, kept by a call) holds the borrow past
        // `a`'s scope: the capture itself is the error.
        let mut captures: Vec<(HirId, LocalId)> = f.borrow_captures.iter().copied().collect();
        captures.sort_by_key(|&(at, l)| (f.order(at), l.0));
        for (lambda, a) in captures {
            if !cow_ty(checker, body.local(a).ty.as_ref()) {
                continue;
            }
            let name = &body.local(a).name;
            match closure_life(body, f, lambda) {
                Ok((holder, span, holders)) => {
                    let holder = holder
                        .map(|h| body.local(h))
                        .filter(|h| !h.name.starts_with('$') && h.kind != LocalKind::Temp)
                        .map_or_else(String::new, |h| h.name.clone());
                    borrows.push(Borrow { name: holder, root: a, path: Vec::new(), text: name.clone(), span, within: Some(lambda), capture: true, local: None, holders });
                }
                Err(why) => {
                    let span = body.expr(lambda).span;
                    let mut m = Message::error(
                        ErrorCode::BorrowConflict,
                        format!("this closure borrows `{name}` (`use (mut {name})`) and {why}: the borrow would outlive `{name}`"),
                        span.0..span.1,
                    );
                    m.with_help(format!("capture a copy with `use ({name})` and hand the changed value back, or hold the shared state in a `Ref<T>`"));
                    self.notes.push(m);
                }
            }
        }
        // In a view's own method, each `mut` field is the borrowed object
        // for the whole body.
        if let Some(&p) = body.params.first()
            && body.kind == crate::hir::BodyKind::Method
            && body.local(p).name == "self"
            && let Some(key) = crate::hir::view_lint::view_key(checker, body.local(p).ty.as_ref())
        {
            for (field, _) in checker.class_fields(&key).unwrap_or_default() {
                if checker.is_borrow_field(&key, &field) {
                    borrows.push(Borrow {
                        name: "self".to_string(),
                        root: p,
                        path: vec![Some(field.clone())],
                        text: format!("self.{field}"),
                        span: (0, u32::MAX - 1),
                        within: None,
                        capture: false,
                        local: None,
                        holders: HashSet::new(),
                    });
                }
            }
        }
        self.borrows = borrows
            .iter()
            .map(|b| Held {
                root: b.root,
                path: b.path.clone(),
                span: b.span,
                within: b.within,
                loose: b.capture && b.within.is_some_and(|l| f.loose_captures.contains(&(l, b.root))),
            })
            .collect();
        if borrows.is_empty() {
            return;
        }
        // Every point that gives a place's object a second name, shared
        // or moved: stores, and bindings other than borrows. Third: how many
        // steps below the place the name is: a `for` over a `Vec` or an
        // array binds an element, not the iterable (#2827).
        let mut shares: Vec<(HirId, HirId, usize)> = self.stored.iter().map(|&v| (v, v, 0)).collect();
        for (&l, inits) in &f.inits {
            if !body.local(l).borrow {
                shares.extend(inits.iter().map(|&v| (v, v, 0)));
            }
        }
        for (&l, &(src, binder)) in &f.bound {
            if !body.local(l).borrow {
                let element = matches!(body.expr(binder).kind, HirKind::ForIn { kind: Some(ForInKind::Array), .. });
                shares.push((src, binder, usize::from(element)));
            }
        }
        shares.sort_by_key(|&(v, _, _)| f.order(v));
        let mut calls: Vec<(HirId, ArgUse)> = f.calls.iter().map(|(&c, &u)| (c, u)).collect();
        calls.sort_by_key(|&(c, _)| f.order(c));
        // `for` loops over an iterator, a `Vec` or an array, in evaluation
        // order: each walks the value its iterable had when it began.
        let mut walks: Vec<HirId> = (0..body.exprs.len())
            .map(|i| HirId(i as u32))
            .filter(|&id| f.order(id) != u32::MAX && (iterator_loop(body, id) || matches!(body.expr(id).kind, HirKind::ForIn { kind: Some(ForInKind::Array), .. })))
            .collect();
        walks.sort_by_key(|&id| f.order(id));
        let mut seen: HashSet<(HirId, String)> = HashSet::new();
        for b in &borrows {
            let (start, end) = b.span;
            let live = |at: HirId| {
                let o = f.order(at);
                o != u32::MAX && o > start && o <= end && !b.within.is_some_and(|w| f.contains(w, at))
            };
            let covers_below = |v: HirId, below: usize| {
                place_path(body, f, v).is_some_and(|(r, mut p)| {
                    p.extend(std::iter::repeat_n(None, below));
                    r == b.root && prefix_of(&p, &b.path)
                })
            };
            let covers = |v: HirId| covers_below(v, 0);
            let stext = &b.text;
            let (bname, last) = match (b.capture, b.name.as_str()) {
                (false, n) => (format!("`{n}`"), format!("use `{n}` for the last time")),
                (true, "") => ("the closure".to_string(), "let the closure go".to_string()),
                (true, n) => (format!("the closure `{n}`"), format!("call `{n}` for the last time")),
            };
            for &(v, at, below) in &shares {
                if live(at) && covers_below(v, below) && !f.borrow_args.contains(&v) && seen.insert((v, stext.clone())) {
                    let span = body.expr(v).span;
                    let vtext = place_text(body, v).unwrap_or_else(|| stext.clone());
                    let mut m = Message::error(
                        ErrorCode::BorrowConflict,
                        format!("`{vtext}` gets a second name while {bname} borrows `{stext}`: writes through {bname} would show through both"),
                        span.0..span.1,
                    );
                    m.with_help(format!("{last} before this, or hold the shared state in a `Ref<T>`"));
                    self.notes.push(m);
                }
            }
            // A call result that may be the borrowed object or one holding
            // it (#2818): bound, stored or passed on, it is a second name.
            for &(c, u) in &calls {
                if !live(c) || f.fresh_calls.contains(&c) {
                    continue;
                }
                let HirKind::Call { args, .. } = &body.expr(c).kind else { continue };
                let Some((named, steps)) = named_result(f, body, c) else { continue };
                if !may_hold_objects(body.expr(named).ty.as_ref()) {
                    continue;
                }
                let eager = self.share.contains(&(c, true));
                let hit = args.iter().enumerate().take(64).any(|(j, &a)| {
                    let bit = 1u64 << j;
                    let v = named_value(body, a);
                    let Some((r, p)) = place_path(body, f, v) else { return false };
                    if r != b.root || !prefix_of(&p, &b.path) {
                        return false;
                    }
                    // How far below the argument the named value is at the
                    // least: the argument itself when the call may hand it
                    // back (a copy of it, past the copy's own object), one
                    // step when the result may be an object inside it.
                    let below = if u.returned & bit != 0 && !self.stored.contains(&v) {
                        usize::from(eager)
                    } else if u.inner & bit != 0 && may_hold_objects(body.expr(v).ty.as_ref()) {
                        1
                    } else {
                        return false;
                    };
                    p.len() + below + steps <= b.path.len()
                });
                if hit && seen.insert((c, stext.clone())) {
                    let span = body.expr(c).span;
                    let mut m = Message::error(
                        ErrorCode::BorrowConflict,
                        format!("this call's result may be `{stext}`'s object, so it gets a second name while {bname} borrows `{stext}`: writes through {bname} would show through both"),
                        span.0..span.1,
                    );
                    m.with_help(format!("{last} before this, or hold the shared state in a `Ref<T>`"));
                    self.notes.push(m);
                }
            }
            // Code not known here given the borrowed object, a holder of it
            // or the borrow itself may keep it (#2825): a second holder,
            // which a later in-place write through the borrow (or the
            // source's, which would copy the object away from the borrow)
            // meets. Refused where such a write may run after the call while
            // the borrow lives.
            let start = b.span.0;
            let written_after = |c: HirId| {
                let loops: Vec<HirId> = f.loops_around(c).into_iter().filter(|&lp| !(f.order(lp) <= start && start < f.end[lp.0 as usize])).collect();
                let after = |at: HirId| at != c && live(at) && (f.order(at) > f.order(c) || loops.iter().any(|&lp| f.contains(lp, at)));
                f.writes.iter().any(|w| {
                    after(w.at)
                        && place_path(body, f, w.path).is_some_and(|(x, steps)| {
                            Some(x) == b.local || (x == b.root && (prefix_of(&steps, &b.path) || prefix_of(&b.path, &steps)))
                        })
                }) || b.holders.iter().any(|h| f.reads.get(h).is_some_and(|rs| rs.iter().any(|&r| after(r))))
            };
            for &(c, u) in &calls {
                if !live(c) || u.opaque == 0 {
                    continue;
                }
                let HirKind::Call { args, .. } = &body.expr(c).kind else { continue };
                let given = args.iter().enumerate().take(64).find(|&(j, &a)| {
                    let v = named_value(body, a);
                    u.opaque & (1u64 << j) != 0
                        && may_hold_objects(body.expr(v).ty.as_ref())
                        && place_path(body, f, v).is_some_and(|(r, p)| (r == b.root && prefix_of(&p, &b.path)) || (Some(r) == b.local && p.is_empty()))
                });
                let Some((_, &a)) = given else { continue };
                if written_after(c) && seen.insert((c, stext.clone())) {
                    let v = named_value(body, a);
                    let span = body.expr(v).span;
                    let vtext = place_text(body, v).unwrap_or_else(|| stext.clone());
                    let mut m = Message::error(
                        ErrorCode::BorrowConflict,
                        format!("`{vtext}` is handed to code not known here, which may keep it, while {bname} borrows `{stext}`: a later write through {bname} would show through what it keeps"),
                        span.0..span.1,
                    );
                    m.with_help(format!("make the borrow's writes before this call and {last} there, or hold the shared state in a `Ref<T>`"));
                    self.notes.push(m);
                }
            }
            for &(capture_at, l) in &self.share_before {
                let at = capture_at.1;
                if l == b.root && b.path.iter().all(Option::is_some) && live(at) && seen.insert((at, stext.clone())) {
                    let span = body.expr(at).span;
                    let name = &body.local(l).name;
                    let mut m = Message::error(
                        ErrorCode::BorrowConflict,
                        format!("this closure captures `{name}` while {bname} borrows `{stext}`: writes through {bname} would show in the closure's copy"),
                        span.0..span.1,
                    );
                    m.with_help(format!("{last} before this closure, or capture with `use (mut {name})`"));
                    self.notes.push(m);
                }
            }
            // A `for` holds what it walks while the body runs (an iterator
            // its iterable, #2819; a loop over a `Vec` or an array a share of
            // it, #2827): over the borrowed object or one holding it, a write in
            // the body to what the loop walks meets that second holder. A
            // write through the source would copy it away from the borrow,
            // one through the borrow would change what the loop walks.
            for &l in &walks {
                let HirKind::ForIn { iterable, body: lbody, .. } = &body.expr(l).kind else { continue };
                // A loop's own `for mut` binding writes the element it is
                // bound to, which the walk has already passed.
                if b.local.is_some_and(|x| f.bound.get(&x).is_some_and(|&(_, binder)| binder == l)) {
                    continue;
                }
                let Some((r, ip)) = place_path(body, f, *iterable) else { continue };
                // The walked place from the borrow's root.
                let walked = if r == b.root && prefix_of(&ip, &b.path) {
                    ip
                } else if Some(r) == b.local && ip.is_empty() {
                    b.path.clone()
                } else {
                    continue;
                };
                let inside = |at: HirId| f.contains(*lbody, at) && live(at);
                let written = f.writes.iter().any(|w| {
                    inside(w.at)
                        && place_path(body, f, w.path).is_some_and(|(x, steps)| {
                            (x == b.root && (prefix_of(&steps, &walked) || prefix_of(&walked, &steps))) || Some(x) == b.local
                        })
                });
                let called = b.holders.iter().any(|h| f.reads.get(h).is_some_and(|rs| rs.iter().any(|&r| inside(r))));
                if (written || called) && seen.insert((l, stext.clone())) {
                    let span = body.expr(*iterable).span;
                    let itext = place_text(body, *iterable).unwrap_or_else(|| stext.clone());
                    let mut m = Message::error(
                        ErrorCode::BorrowConflict,
                        format!("the `for` over `{itext}` holds its object while its body writes it and {bname} borrows `{stext}`: the loop and {bname} would see different objects"),
                        span.0..span.1,
                    );
                    m.with_help(format!("{last} before the loop, or keep what the loop finds and write it after the loop"));
                    self.notes.push(m);
                }
            }
            for (i, e) in body.exprs.iter().enumerate() {
                let id = HirId(i as u32);
                let HirKind::Assign { place, .. } = &e.kind else { continue };
                if live(id) && covers(*place) && seen.insert((id, stext.clone())) {
                    let span = e.span;
                    let ptext = place_text(body, *place).unwrap_or_else(|| stext.clone());
                    let mut m = Message::error(
                        ErrorCode::BorrowConflict,
                        format!("`{ptext}` is replaced while {bname} borrows `{stext}`: writes through {bname} would go to the old object"),
                        span.0..span.1,
                    );
                    let again = if b.capture { "make the closure after it" } else { "borrow again after it" };
                    m.with_help(format!("{last} before this, or {again}"));
                    self.notes.push(m);
                }
            }
        }
    }

    /// A write through a borrowed place while the borrow lives (#2816)
    /// leaves the borrowed object, and each object holding it, as it is:
    /// it is written in place, as it is through the borrow. The borrow made
    /// them unique where it began and E0149 keeps every other name off them
    /// while it lives, so a shared flag there was left by a call that kept
    /// nothing (a callee walking the object: its iterator shared it and died
    /// with the call). A unique would copy the object away from the borrow.
    /// Not inside a loop over an iterator, which holds its iterable while
    /// the loop runs.
    fn hold_borrowed(&mut self, body: &HirBody, f: &Facts<'_>) {
        let live = |b: &Held, at: HirId| {
            let o = f.order(at);
            o != u32::MAX && o > b.span.0 && o <= b.span.1 && !b.within.is_some_and(|w| f.contains(w, at))
        };
        // A call in the borrow's life that hands the borrowed object, or one
        // holding it, to code not known here (which may keep it), or whose
        // result may be an object inside a holder (the borrowed one), or a
        // closure body that may keep its capture: the object may have a
        // holder E0149 does not see, so writes keep their unique.
        let open = |b: &Held| {
            b.loose
                || f.calls.iter().any(|(&c, u)| {
                    let HirKind::Call { args, .. } = &body.expr(c).kind else { return false };
                    live(b, c)
                        && args.iter().enumerate().take(64).any(|(j, &a)| {
                            let bit = 1u64 << j;
                            if (u.opaque | u.inner) & bit == 0 {
                                return false;
                            }
                            match place_path(body, f, named_value(body, a)) {
                                Some((x, p)) if x == b.root => {
                                    prefix_of(&p, &b.path) && (u.opaque & bit != 0 || p.len() < b.path.len())
                                }
                                Some((x, _)) => aliases(f, body, x, b.root),
                                None => false,
                            }
                        })
                })
        };
        // Any such call during one borrow of a root ends the others' too.
        let open_roots: HashSet<LocalId> = self.borrows.iter().filter(|b| open(b)).map(|b| b.root).collect();
        let borrows: Vec<&Held> = self.borrows.iter().filter(|b| !open_roots.contains(&b.root)).collect();
        if borrows.is_empty() {
            return;
        }
        let mut held = HashMap::new();
        for &(w, _) in &self.unique {
            let Some((root, steps)) = write_steps(body, w.path) else { continue };
            if f.order(w.at) == u32::MAX || in_iterator_loop(body, f, w.at) {
                continue;
            }
            let mut n = 0;
            for b in &borrows {
                if b.root == root && live(b, w.at) {
                    let common = steps.iter().zip(&b.path).take_while(|(s, p)| s.is_some() && s == p).count();
                    n = n.max(1 + common);
                }
            }
            if n > 0 {
                held.insert((w.at, w.path), n);
            }
        }
        self.held = held;
    }

    /// E0141 where a write copies an object a binding shares with its
    /// source, E0142 where a method writes a temporary and the change is
    /// dropped, E0143 where a closure assigns a `use (mut x)` capture.
    fn note_copies(&mut self, checker: &Checker, body: &HirBody, f: &Facts<'_>, copied_from: &HashMap<LocalId, HirId>) {
        // `mut` borrows the capture's object; the closure's own word is
        // still a copy, so assigning it never reaches the outer local.
        for &at in &f.capture_assigns {
            let HirKind::Assign { place, .. } = body.expr(at).kind else { continue };
            let HirKind::Local(l) = body.expr(place).kind else { continue };
            let name = &body.local(l).name;
            let span = body.expr(at).span;
            let mut m = Message::warn(
                ErrorCode::CaptureAssign,
                format!("this assignment changes the closure's `{name}` only: the outer `{name}` keeps its value"),
                span.0..span.1,
            );
            m.with_help(format!(
                "`use (mut {name})` shares `{name}`'s object, not the variable; hold the value in a `Ref<T>` to change it from the closure"
            ));
            self.notes.push(m);
        }
        let mut firsts: HashMap<LocalId, Write> = HashMap::new();
        for &w in &f.writes {
            let Some(r) = f.root_of(w.path) else { continue };
            if let HirKind::Local(l) = body.expr(r).kind
                && copied_from.contains_key(&l)
                && firsts.get(&l).is_none_or(|o| f.order(w.at) < f.order(o.at))
            {
                firsts.insert(l, w);
            }
            // A method that writes a call's result: nothing keeps the change.
            if let HirKind::Call { callee: Callee::Method { name }, args } = &body.expr(w.at).kind
                && args.first() == Some(&w.path)
                && w.path == r
                && matches!(body.expr(r).kind, HirKind::Call { .. })
                && !fresh_ctor(body, r)
                && f.calls.get(&w.at).is_some_and(|u| u.opaque == 0)
                && cow_ty(checker, body.expr(r).ty.as_ref())
                && !open_param(checker, body.expr(r).ty.as_ref())
                && f.parent.get(&w.at).is_some_and(|p| matches!(&body.expr(*p).kind, HirKind::Block { stmts, .. } if stmts.contains(&w.at)))
            {
                let span = body.expr(w.at).span;
                let mut m = Message::warn(
                    ErrorCode::CowDroppedWrite,
                    format!("`.{name}()` changes a temporary copy; the change is dropped"),
                    span.0..span.1,
                );
                m.with_help("the object is copied before the write because something else shares it; write to a variable (`let x = mut …`) to keep the change, or `let _ = …` if only the call's other effects matter".to_string());
                self.notes.push(m);
            }
        }
        // A write through a copied capture copies it on every call: the
        // closure's own object never changes.
        let mut captured: HashMap<LocalId, Write> = HashMap::new();
        for &w in &f.writes {
            let Some(r) = f.root_of(w.path) else { continue };
            if let HirKind::Local(l) = body.expr(r).kind
                && body.local(l).kind == LocalKind::Capture
                && !body.local(l).borrow
                && cow_ty(checker, body.local(l).ty.as_ref())
                && !open_param(checker, body.local(l).ty.as_ref())
                && captured.get(&l).is_none_or(|o| f.order(w.at) < f.order(o.at))
            {
                captured.insert(l, w);
            }
        }
        let mut captured: Vec<(LocalId, Write)> = captured.into_iter().collect();
        captured.sort_by_key(|(_, w)| f.order(w.at));
        for (l, w) in captured {
            let name = &body.local(l).name;
            let span = body.expr(w.at).span;
            let mut m = Message::warn(
                ErrorCode::CowCopy,
                format!("this write copies the captured `{name}` on every call: a closure captures a copy"),
                span.0..span.1,
            );
            m.with_help(format!(
                "write `use (mut {name})` to change the outer `{name}`, or hold state the closure keeps between calls in a `Ref<T>`"
            ));
            self.notes.push(m);
        }
        let mut firsts: Vec<(LocalId, Write)> = firsts.into_iter().collect();
        firsts.sort_by_key(|(_, w)| f.order(w.at));
        for (l, w) in firsts {
            let Some(src) = place_text(body, copied_from[&l]) else { continue };
            let name = &body.local(l).name;
            if name.starts_with('$') || open_param(checker, body.local(l).ty.as_ref()) {
                continue;
            }
            let span = body.expr(w.at).span;
            let looped = !f.loops_around(w.at).is_empty() && !f.loops_around(copied_from[&l]).is_empty();
            let mut m = Message::warn(
                ErrorCode::CowCopy,
                if looped {
                    format!("this write copies `{name}` on every loop iteration: it shares its object with `{src}`")
                } else {
                    format!("this write copies `{name}`: it shares its object with `{src}`")
                },
                span.0..span.1,
            );
            // A loop or pattern binding is borrowed where it is bound.
            let binder = f.bound.get(&l).map(|&(_, b)| &body.expr(b).kind);
            let borrow = match binder {
                Some(HirKind::ForIn { .. }) => format!("`for mut {name} in {src}`"),
                None => format!("`let {name} = mut {src}`"),
                _ => "a `mut` binding".to_string(),
            };
            m.with_help(format!(
                "write {borrow} to change `{src}`'s object in place, stop using `{src}` before the write, or hold shared state in a `Ref<T>`"
            ));
            self.notes.push(m);
        }
    }

    fn share_value(&mut self, body: &HirBody, f: &Facts<'_>, v: HirId, inout: &HashSet<LocalId>) {
        let eager = matches!(body.expr(v).kind, HirKind::Local(l) if inout.contains(&l));
        let _ = f;
        self.share.push((v, eager));
    }

    /// `v` is the body's result. A parameter handed back as itself is the
    /// caller's to share (its summary's `returns_arg`, #2206): the caller
    /// knows whether the argument's place lives on. Anything else is a
    /// store, as for [`Self::store`].
    #[allow(clippy::too_many_arguments)]
    fn store_result(&mut self, body: &HirBody, f: &Facts<'_>, v: HirId, cow: &dyn Fn(HirId) -> bool, inout: &HashSet<LocalId>, borrowed: &HashSet<LocalId>) {
        if body.is_coro {
            return self.store(body, f, v, cow, inout, borrowed);
        }
        let mut work = vec![v];
        while let Some(at) = work.pop() {
            match &body.expr(at).kind {
                HirKind::Block { tail: Some(t), .. } => work.push(*t),
                HirKind::If { then, els, .. } => work.extend(std::iter::once(*then).chain(*els)),
                HirKind::Match { arms, .. } => work.extend(arms.iter().map(|a| a.body)),
                HirKind::Local(l) if cow(at) && body.local(*l).kind == LocalKind::Param && !body.local(*l).borrow => {
                    self.stored.push(at);
                }
                // A parameter's field or element: the caller shares the
                // result where it keeps it (the summary's `returns_inner`).
                _ if cow(at) && inner_param(body, at).is_some() => self.stored.push(at),
                _ => self.store(body, f, at, cow, inout, borrowed),
            }
        }
    }

    /// `v` is stored where it outlives this read: share it unless it is a
    /// fresh value or the last read of a local that owns its object.
    #[allow(clippy::too_many_arguments)]
    fn store(&mut self, body: &HirBody, f: &Facts<'_>, v: HirId, cow: &dyn Fn(HirId) -> bool, inout: &HashSet<LocalId>, borrowed: &HashSet<LocalId>) {
        let mut work = vec![v];
        while let Some(at) = work.pop() {
            match &body.expr(at).kind {
                HirKind::Block { tail: Some(t), .. } => work.push(*t),
                HirKind::If { then, els, .. } => work.extend(std::iter::once(*then).chain(*els)),
                HirKind::Match { arms, .. } => work.extend(arms.iter().map(|a| a.body)),
                HirKind::Local(l) if cow(at) => {
                    self.stored.push(at);
                    if !borrowed.contains(l) && f.last_read(*l, at) && owned(f, body, *l, inout) {
                        continue;
                    }
                    self.share_value(body, f, at, inout);
                }
                HirKind::Field { .. } | HirKind::Index { .. } | HirKind::Global { .. } if cow(at) => {
                    self.stored.push(at);
                    self.share_value(body, f, at, inout);
                }
                _ => {}
            }
        }
    }
}

/// Whether a value of `ty` can hold objects a callee could return from
/// inside it: not a scalar, a `string` or a function.
fn may_hold_objects(ty: Option<&Ty>) -> bool {
    match ty.map(strip_readonly) {
        Some(Ty::Con(n)) if n == "string" => false,
        Some(Ty::Fun(..)) => false,
        t => !super::escape::no_heap(t),
    }
}

/// Whether `v` may read a named place. A call's result, a new object or
/// a literal is a temporary no other name reaches: fresh, or shared where
/// it was made.
fn is_place(body: &HirBody, v: HirId) -> bool {
    !matches!(body.expr(v).kind, HirKind::Call { .. } | HirKind::Make { .. } | HirKind::Lit(_))
}

/// Whether a call's result is only read where it is made: dropped as a
/// statement or by `let _`, or an operand of an operator.
fn read_only_result(f: &Facts<'_>, body: &HirBody, call: HirId) -> bool {
    let Some(&p) = f.parent.get(&call) else { return false };
    match &body.expr(p).kind {
        HirKind::Block { stmts, .. } => stmts.contains(&call),
        HirKind::LetPat { pat: HirPat::Wild, .. } => true,
        HirKind::Let { local, .. } => body.local(*local).name == "_",
        HirKind::Bin { .. } | HirKind::Un { .. } => true,
        _ => false,
    }
}

/// Whether a write through local `l` other than `call`'s own may run after
/// `call`: later in the body, or anywhere in a loop around it.
fn written_after(f: &Facts<'_>, body: &HirBody, l: LocalId, call: HirId) -> bool {
    let loops = f.loops_around(call);
    f.writes.iter().any(|w| {
        w.at != call
            && f.order(w.at) != u32::MAX
            && f.root_of(w.path).is_some_and(|r| matches!(body.expr(r).kind, HirKind::Local(x) if x == l))
            && (f.order(w.at) > f.order(call) || loops.iter().any(|&lp| f.contains(lp, w.at)))
    })
}

/// Where a call's result is given a name (#2818): the node that is bound,
/// stored or passed on, and how many field or element steps below the
/// result it reads. `None` where the result is only read: dropped, an
/// operand, or an argument a callee neither keeps nor hands back.
fn named_result(f: &Facts<'_>, body: &HirBody, call: HirId) -> Option<(HirId, usize)> {
    let mut at = call;
    let mut steps = 0;
    loop {
        if read_only_result(f, body, at) {
            return None;
        }
        let Some(&p) = f.parent.get(&at) else { return Some((at, steps)) };
        match &body.expr(p).kind {
            HirKind::Field { base, .. } | HirKind::Index { base, kind: IndexKind::Array, .. } if *base == at => {
                // The place of a write: the write makes the temporary unique.
                if matches!(f.parent.get(&p).map(|g| &body.expr(*g).kind), Some(HirKind::Assign { place, .. }) if *place == p) {
                    return None;
                }
                steps += 1;
                at = p;
            }
            HirKind::Call { args, .. } => {
                let Some(j) = args.iter().position(|&a| named_value(body, a) == at) else { return Some((at, steps)) };
                let u = f.calls.get(&p).copied().unwrap_or_default();
                let bit = if j < 64 { 1u64 << j } else { u64::MAX };
                return ((u.kept | u.opaque | u.returned | u.inner) & bit != 0).then_some((at, steps));
            }
            _ => return Some((at, steps)),
        }
    }
}

/// Whether argument `v` of `call` is moved into it (#2206): its place is
/// not read again while the result may name its object. Either the last
/// read of a local that owns its object, or `l = f(.., l, ..)` with `l`
/// read once in the call: the assignment overwrites the place with the
/// result before anything reads it.
fn moved_into_call(f: &Facts<'_>, body: &HirBody, call: HirId, v: HirId, inout: &HashSet<LocalId>, borrowed: &HashSet<LocalId>) -> bool {
    // A temporary has no other name to read it after the call.
    if !is_place(body, v) {
        return true;
    }
    let HirKind::Local(l) = body.expr(v).kind else { return place_moved_into_call(f, body, call, v, borrowed) };
    let local = body.local(l);
    if borrowed.contains(&l) || local.borrow || local.kind == LocalKind::Capture || f.captured.contains(&l) {
        return false;
    }
    if f.last_read(l, v) && owned(f, body, l, inout) {
        return true;
    }
    let assigned_back = matches!(
        f.parent.get(&call).map(|p| &body.expr(*p).kind),
        Some(HirKind::Assign { place, value }) if *value == call && matches!(body.expr(*place).kind, HirKind::Local(x) if x == l)
    );
    assigned_back && f.reads.get(&l).is_some_and(|rs| rs.iter().filter(|&&r| f.contains(call, r)).count() == 1)
}

/// Whether argument `v` is a field or element place the call's result is
/// assigned straight back to (`self.f = g(self.f)`, `a[i] = h(a[i])`,
/// #2206): the old value is dead once the call returns, so it moves in.
/// The root may not be borrowed, captured or read elsewhere in the call,
/// and an index must be a literal or a plain local the call cannot change.
fn place_moved_into_call(f: &Facts<'_>, body: &HirBody, call: HirId, v: HirId, borrowed: &HashSet<LocalId>) -> bool {
    let Some(HirKind::Assign { place, value }) = f.parent.get(&call).map(|p| &body.expr(*p).kind) else { return false };
    if *value != call || !matches!(body.expr(v).kind, HirKind::Field { .. } | HirKind::Index { .. }) {
        return false;
    }
    let plain = |l: LocalId| {
        let local = body.local(l);
        !borrowed.contains(&l) && !local.borrow && local.kind != LocalKind::Capture && !f.captured.contains(&l)
    };
    let mut roots = Vec::new();
    if !same_place(body, *place, v, &plain, &mut roots) {
        return false;
    }
    // Nothing else in the call reads the root or an index: the argument
    // is the only way the call sees them.
    roots.iter().all(|l| f.reads.get(l).is_none_or(|rs| rs.iter().all(|&r| !f.contains(call, r) || f.contains(v, r))))
}

/// Whether `a` and `b` name the same place through the same field names
/// and equal indices (literals, or the same place again), every local in
/// them plain; the locals go to `roots`.
fn same_place(body: &HirBody, a: HirId, b: HirId, plain: &dyn Fn(LocalId) -> bool, roots: &mut Vec<LocalId>) -> bool {
    match (&body.expr(a).kind, &body.expr(b).kind) {
        (HirKind::Field { base: ab, name: an }, HirKind::Field { base: bb, name: bn }) => an == bn && same_place(body, *ab, *bb, plain, roots),
        (HirKind::Index { base: ab, index: ai, kind: IndexKind::Array }, HirKind::Index { base: bb, index: bi, kind: IndexKind::Array }) => {
            let index_eq = match (&body.expr(*ai).kind, &body.expr(*bi).kind) {
                (HirKind::Lit(Lit::Int(i)), HirKind::Lit(Lit::Int(j))) => i == j,
                _ => same_place(body, *ai, *bi, plain, roots),
            };
            index_eq && same_place(body, *ab, *bb, plain, roots)
        }
        (HirKind::Builtin { op: Builtin::Cow { .. }, args }, _) if args.len() == 1 => same_place(body, args[0], b, plain, roots),
        (_, HirKind::Builtin { op: Builtin::Cow { .. }, args }) if args.len() == 1 => same_place(body, a, args[0], plain, roots),
        (HirKind::Local(x), HirKind::Local(y)) if x == y && plain(*x) => {
            roots.push(*x);
            true
        }
        _ => false,
    }
}

/// A place as its root local and the steps below it: a field name, or
/// `None` for an element (any index).
fn place_path(body: &HirBody, f: &Facts<'_>, v: HirId) -> Option<(LocalId, Vec<Option<String>>)> {
    let _ = f;
    let mut steps = Vec::new();
    let mut at = v;
    loop {
        match &body.expr(at).kind {
            HirKind::Field { base, name } => {
                steps.push(Some(name.clone()));
                at = *base;
            }
            HirKind::Index { base, kind: IndexKind::Array, .. } => {
                steps.push(None);
                at = *base;
            }
            HirKind::Builtin { op: Builtin::Cow { .. }, args } if args.len() == 1 => at = args[0],
            HirKind::Local(l) => {
                steps.reverse();
                return Some((*l, steps));
            }
            _ => return None,
        }
    }
}

/// A write path as [`Plan::unique_path`] walks it (fields and array
/// elements): its root local and the steps below it, `None` for an element.
fn write_steps(body: &HirBody, path: HirId) -> Option<(LocalId, Vec<Option<String>>)> {
    let mut steps = Vec::new();
    let mut at = path;
    loop {
        match &body.expr(at).kind {
            HirKind::Field { base, name } => {
                steps.push(Some(name.clone()));
                at = *base;
            }
            HirKind::Index { base, kind: IndexKind::Array, .. } => {
                steps.push(None);
                at = *base;
            }
            HirKind::Local(l) => {
                steps.reverse();
                return Some((*l, steps));
            }
            _ => return None,
        }
    }
}

/// Whether the `for` at `id` may keep what it walks past the loop, other
/// than in its iterator, which dies with the loop (#2821): its `into_iter`
/// or `next` keeps what it is given somewhere other than its result, or
/// hands it to code not known here, or `next` may hand back the walked
/// object itself. A `for` is no `Call` in HIR, so this is the call's
/// `kept` for it. An array, tuple, range or generator loop keeps nothing
/// (a generator's own frame keeps what it was given).
fn walk_keeps(module: &HirModule, body: &HirBody, lookup: &CallLookup<'_>, id: HirId) -> bool {
    let HirKind::ForIn { pat, iterable, kind, .. } = &body.expr(id).kind else { return false };
    let Some(ForInKind::Custom { into_iter_fqn, next_fqn, .. }) = kind else {
        return kind.is_none();
    };
    // An instance method by the key the loop names: this module's body, or
    // an earlier module's through the lookup.
    let summary = |key: &str| {
        let name = module.instance_fns.get(key).map_or_else(|| key.to_string(), |&i| module.bodies[i].name.clone());
        lookup(&Callee::Named { name, def: None, overload: None }, &[])
    };
    let loose = |s: &Option<Summary>| s.is_none_or(|s| (s.escapes | s.opaque_args) & 1 != 0);
    if into_iter_fqn.as_deref().is_some_and(|k| loose(&summary(k))) {
        return true;
    }
    let Some(next) = next_fqn else { return false };
    let s = summary(next);
    if loose(&s) {
        return true;
    }
    // An object inside the iterator handed back: the walked one itself
    // when the item's type is the iterable's.
    let s = s.unwrap_or_default();
    let mut itself = false;
    if (s.returns_inner | s.returns_arg) & 1 != 0 {
        let walked = body.expr(*iterable).ty.as_ref();
        pat.for_each_bind(&mut |l| itself |= may_be_same_type(body.local(l).ty.as_ref(), walked));
    }
    itself
}

/// Whether a value of type `a` may be one of type `b`: the same head, or
/// either not known.
fn may_be_same_type(a: Option<&Ty>, b: Option<&Ty>) -> bool {
    let head = |t: &Ty| match strip_readonly(t) {
        Ty::App(h, _) => Some(h.as_ref().clone()),
        Ty::Var(_) => None,
        t => Some(t.clone()),
    };
    match (a.and_then(head), b.and_then(head)) {
        (Some(x), Some(y)) => x == y,
        _ => true,
    }
}

/// Whether `id` runs inside a `for` over an iterator (not an array, a
/// tuple or a range), whose iterator holds the iterable while it runs.
fn in_iterator_loop(body: &HirBody, f: &Facts<'_>, id: HirId) -> bool {
    let mut at = id;
    while let Some(&p) = f.parent.get(&at) {
        if iterator_loop(body, p) && matches!(&body.expr(p).kind, HirKind::ForIn { body: b, .. } if *b == at) {
            return true;
        }
        at = p;
    }
    false
}

/// Whether write `w` runs in the body of a `for` that holds its iterable
/// (one over an iterator, or a `holding` one over a `Vec` or an array)
/// whose iterable is a place with the write's root local.
fn walked_write(body: &HirBody, f: &Facts<'_>, w: Write, holding: &HashSet<HirId>) -> bool {
    let Some((root, _)) = place_path(body, f, w.path) else { return false };
    let mut at = w.at;
    while let Some(&p) = f.parent.get(&at) {
        if let HirKind::ForIn { iterable, body: b, .. } = &body.expr(p).kind
            && *b == at
            && (iterator_loop(body, p) || holding.contains(&p))
            && place_path(body, f, *iterable).is_some_and(|(r, _)| r == root)
        {
            return true;
        }
        at = p;
    }
    false
}

/// Whether the body of the `for` at `id` over a `Vec`, an array or an
/// iterator may write the object it walks (#2827): a write through a path from the
/// iterable's root that leads to it, below it or to an object holding it
/// (a store into another field of a holder does not), a call that may
/// write one of those (code not known here counts), or any write through
/// a borrow of the root other than the loop's own `for mut` binding. Over a `static`, a write to it or a call that may
/// write a `static`.
fn walk_written(body: &HirBody, f: &Facts<'_>, id: HirId) -> bool {
    let HirKind::ForIn { iterable, body: lbody, kind, .. } = &body.expr(id).kind else { return false };
    if !matches!(kind, Some(ForInKind::Array)) && !iterator_loop(body, id) {
        return false;
    }
    let inside = |at: HirId| f.contains(*lbody, at);
    let Some((r, ip)) = place_path(body, f, *iterable) else {
        let Some(HirKind::Global { name, .. }) = f.root_of(*iterable).map(|g| &body.expr(g).kind) else { return false };
        let same = |w: &Write| f.root_of(w.path).is_some_and(|x| matches!(&body.expr(x).kind, HirKind::Global { name: n, .. } if n == name));
        return f.writes.iter().any(|w| inside(w.at) && same(w)) || f.static_calls.iter().any(|&c| inside(c));
    };
    f.writes.iter().any(|&w| {
        inside(w.at)
            && write_reach(body, f, w).is_some_and(|(x, steps)| {
                if x == r {
                    prefix_of(&steps, &ip) || prefix_of(&ip, &steps)
                } else {
                    // The loop's own `for mut` binding writes the element it
                    // is bound to, which the walk has already read.
                    let own = f.bound.get(&x).is_some_and(|&(_, binder)| binder == id);
                    !own && body.local(x).borrow && aliases(f, body, x, r)
                }
            })
    })
}

/// The place write `w` changes: its root local and the steps to the object
/// it writes, one step further for a store into a field or an element (a
/// store into `r.n` changes `r`'s slot `n`, not what `r.m` holds).
fn write_reach(body: &HirBody, f: &Facts<'_>, w: Write) -> Option<(LocalId, Vec<Option<String>>)> {
    let (root, mut steps) = place_path(body, f, w.path)?;
    if let HirKind::Assign { place, .. } = &body.expr(w.at).kind {
        match &body.expr(*place).kind {
            HirKind::Field { base, name } if *base == w.path => steps.push(Some(name.clone())),
            HirKind::Index { base, .. } if *base == w.path => steps.push(None),
            _ => {}
        }
    }
    Some((root, steps))
}

/// Whether `id` is a `for` over an iterator (not an array, a tuple or a
/// range): its iterator holds the iterable while the loop runs.
fn iterator_loop(body: &HirBody, id: HirId) -> bool {
    matches!(&body.expr(id).kind, HirKind::ForIn { kind, .. } if !matches!(kind, Some(ForInKind::Array | ForInKind::Tuple { .. } | ForInKind::Range { .. })))
}

/// Whether place steps `a` lead to `b` or to an object holding it.
/// Elements match any index.
fn prefix_of(a: &[Option<String>], b: &[Option<String>]) -> bool {
    a.len() <= b.len() && a.iter().zip(b).all(|(x, y)| x.is_none() || y.is_none() || x == y)
}

/// Whether `x` is bound from a place rooted at `root` (directly or
/// through other such bindings).
fn aliases(f: &Facts<'_>, body: &HirBody, x: LocalId, root: LocalId) -> bool {
    let mut seen = HashSet::new();
    let mut work = vec![x];
    while let Some(l) = work.pop() {
        if !seen.insert(l) {
            continue;
        }
        if l == root {
            return true;
        }
        let srcs = f.inits.get(&l).into_iter().flatten().chain(f.bound.get(&l).map(|(s, _)| s));
        for &s in srcs {
            if let Some(r) = f.root_of(s)
                && let HirKind::Local(m) = body.expr(r).kind
            {
                work.push(m);
            }
        }
    }
    false
}

/// Whether local `l` owns its object: not a parameter or capture, and
/// every value it is given is fresh or shared where it was read.
fn owned(f: &Facts<'_>, body: &HirBody, l: LocalId, inout: &HashSet<LocalId>) -> bool {
    let local = body.local(l);
    if matches!(local.kind, LocalKind::Param | LocalKind::Capture) || inout.contains(&l) || f.captured.contains(&l) {
        return false;
    }
    if f.bound.contains_key(&l) {
        return false;
    }
    f.inits.get(&l).into_iter().flatten().all(|&v| !matches!(body.expr(v).kind, HirKind::Local(_) | HirKind::Field { .. } | HirKind::Index { .. } | HirKind::Global { .. }))
}

/// The nodes under `at` other than `id` with `id`'s span and kind.
fn twins_of(body: &HirBody, at: HirId, id: HirId) -> Vec<HirId> {
    let e = body.expr(id);
    let mut out = Vec::new();
    let mut work = vec![at];
    while let Some(n) = work.pop() {
        let x = body.expr(n);
        if n != id && x.span == e.span && std::mem::discriminant(&x.kind) == std::mem::discriminant(&e.kind) {
            out.push(n);
            continue;
        }
        if n != id {
            work.extend(body.children(n));
        }
    }
    out
}

/// Whether code emission packs `id` as an existential (by node or span).
fn pack_site(checker: &Checker, body: &HirBody, id: HirId) -> bool {
    let e = body.expr(id);
    e.node.is_some_and(|n| checker.existential_pack_at(n).is_some()) || checker.existential_pack_for_span(e.span.0, e.span.1).is_some()
}

/// A value a constructor made just now: `new C(..)`, an array literal, an
/// empty or sized `Vec`.
fn fresh_ctor(body: &HirBody, v: HirId) -> bool {
    match &body.expr(v).kind {
        HirKind::Make { kind: MakeKind::Class(_) | MakeKind::Array, .. } => true,
        HirKind::Call { callee: Callee::Named { name, .. }, .. } => {
            matches!(name.as_str(), "Vec::new" | "Vec::with_capacity" | "Vec::from")
        }
        _ => false,
    }
}

impl Plan {
    fn apply(&self, out: &mut HirBody, checker: &Checker) {
        let span_of = |b: &HirBody, id: HirId| b.expr(id).span;
        // Write paths first, on the nodes as built: shares below wrap
        // nodes in place, which a path must not look through.
        // Each before the statement that holds its write, so an operand
        // keeps its shape (unique is idempotent: running it a little
        // earlier changes nothing a program sees).
        let mut before = Before::default();
        for &(w, at) in &self.unique {
            let held = self.held.get(&(w.at, w.path)).copied().unwrap_or(0);
            let walked = self.walked.contains(&(w.at, w.path));
            for s in self.unique_path(out, checker, w.path, w.at, held, walked) {
                before.add(at, s);
            }
        }
        before.apply(out);
        // Pattern-bound and captured locals: a share statement. A `let`
        // pattern binding more than one shares each after it (#2744).
        let mut let_pats: HashSet<HirId> = HashSet::new();
        for &(binder, l) in &self.share_after {
            let span = span_of(out, binder);
            let stmt = share_local(out, l, span);
            if let_pats.contains(&binder) {
                if let HirKind::Block { stmts, .. } = &mut out.exprs[binder.0 as usize].kind {
                    stmts.push(stmt);
                }
                continue;
            }
            match out.expr(binder).kind.clone() {
                HirKind::ForIn { pat, iterable, body: b, kind } => {
                    let inner = relocate(out, b);
                    out.exprs[b.0 as usize].kind = HirKind::Block { stmts: vec![stmt], tail: Some(inner) };
                    out.exprs[b.0 as usize].node = None;
                    let _ = (pat, iterable, kind);
                }
                HirKind::LetPat { .. } => {
                    let_pats.insert(binder);
                    let inner = relocate(out, binder);
                    let x = &mut out.exprs[binder.0 as usize];
                    x.kind = HirKind::Block { stmts: vec![inner, stmt], tail: None };
                    x.node = None;
                }
                // A `match` arm body.
                _ => {
                    let inner = relocate(out, binder);
                    let x = &mut out.exprs[binder.0 as usize];
                    x.kind = HirKind::Block { stmts: vec![stmt], tail: Some(inner) };
                    x.node = None;
                }
            }
        }
        let mut before = Before::default();
        for &(at, l) in &self.share_before {
            let span = span_of(out, at.1);
            let stmt = share_local(out, l, span);
            before.add(at, stmt);
        }
        for &(at, l) in &self.unique_before {
            let span = span_of(out, at.1);
            let ty = out.local(l).ty.clone();
            let read = out.push_new(HirKind::Local(l), ty, span);
            let stmt = unique_stmt(out, read, span);
            before.add(at, stmt);
        }
        before.apply(out);
        for &(v, eager) in &self.share {
            // Code emission finds an existential pack by the value's node
            // or span, which a wrapper would share.
            if pack_site(checker, out, v) {
                continue;
            }
            let inner = relocate(out, v);
            let x = &mut out.exprs[v.0 as usize];
            x.kind = HirKind::Builtin { op: Builtin::Cow { unique: false }, args: vec![inner] };
            x.node = None;
            if eager {
                let shared = relocate(out, v);
                let x = &mut out.exprs[v.0 as usize];
                x.kind = HirKind::Builtin { op: Builtin::Cow { unique: true }, args: vec![shared] };
            }
        }
    }

    /// The statements that make the objects on `path` writable, top-down,
    /// writing each copy back where it was read. The first `held` objects
    /// from the root are a borrow's, written in place ([`Self::hold_borrowed`]).
    /// A `walked` write's root is held by a loop's iterator.
    #[allow(clippy::too_many_arguments)]
    fn unique_path(&self, out: &mut HirBody, checker: &Checker, path: HirId, stmt: HirId, held: usize, walked: bool) -> Vec<HirId> {
        // A copy of an existential pack site would share its span, and
        // code emission would pack the copy too.
        if pack_site(checker, out, path) {
            return Vec::new();
        }
        // The chain from the root to `path`.
        let mut chain = vec![path];
        let mut at = path;
        while let HirKind::Field { base, .. } | HirKind::Index { base, kind: IndexKind::Array, .. } = &out.expr(at).kind {
            at = *base;
            chain.push(at);
        }
        chain.reverse();
        let span = out.expr(path).span;
        let mut stmts = Vec::new();
        // An assignment runs its value before the place's base and index:
        // a base or index evaluated ahead of the statement takes the value
        // with it (not a compound one, whose value reads the place).
        let root_temp = match &out.expr(chain[0]).kind {
            HirKind::Local(_) | HirKind::Global { .. } => false,
            _ => !(chain.len() == 1 && (fresh_ctor(out, chain[0]) || pack_site(checker, out, chain[0]) || !cow_ty(checker, out.expr(chain[0]).ty.as_ref()))),
        };
        let impure_index = chain[1..].iter().any(|&st| matches!(out.expr(st).kind, HirKind::Index { index, .. } if !pure_index(out, index)));
        if (root_temp || impure_index)
            && let HirKind::Assign { value, .. } = out.expr(stmt).kind
            && !pure_index(out, value)
            && chain.iter().all(|&st| twins_of(out, value, st).is_empty())
        {
            let t = out.temp("$cowv", out.expr(value).ty.clone());
            let moved = relocate(out, value);
            stmts.push(out.push_new(HirKind::Let { local: t, init: Some(moved) }, Some(ty::unit()), span));
            let x = &mut out.exprs[value.0 as usize];
            x.kind = HirKind::Local(t);
            x.node = None;
        }
        // An index that is not safe to evaluate twice is evaluated once,
        // into a temporary, before the path.
        for &step in &chain[1..] {
            if let HirKind::Index { index, .. } = out.expr(step).kind.clone()
                && !pure_index(out, index)
            {
                let t = out.temp("$cowi", out.expr(index).ty.clone());
                // A compound assignment reads the place again through a
                // twin of the index (same span and shape), which lowering
                // pairs with this one: it reads the temporary too.
                let twins = twins_of(out, stmt, index);
                let moved = relocate(out, index);
                for twin in twins {
                    let x = &mut out.exprs[twin.0 as usize];
                    x.kind = HirKind::Local(t);
                    x.node = None;
                }
                stmts.push(out.push_new(HirKind::Let { local: t, init: Some(moved) }, Some(ty::unit()), span));
                let x = &mut out.exprs[index.0 as usize];
                x.kind = HirKind::Local(t);
                x.node = None;
            }
        }
        // A root that is no place (a call's result) is a temporary.
        let root = chain[0];
        match &out.expr(root).kind {
            HirKind::Local(l) => {
                let l = *l;
                let skip = held > 0 || (self.fresh_locals.contains(&l) && !walked) || self.inout.contains(&l) || self.defer_captured.contains(&l) || !cow_ty(checker, out.local(l).ty.as_ref());
                if !skip {
                    stmts.push(unique_stmt(out, root, span));
                }
            }
            HirKind::Global { .. } => {
                if cow_ty(checker, out.expr(root).ty.as_ref()) {
                    stmts.push(unique_stmt(out, root, span));
                }
            }
            _ if chain.len() == 1 && (fresh_ctor(out, root) || pack_site(checker, out, root) || !cow_ty(checker, out.expr(root).ty.as_ref())) => {}
            _ => {
                let ty = out.expr(root).ty.clone();
                let t = out.temp("$cowt", ty.clone());
                let moved = relocate(out, root);
                let init = if cow_ty(checker, ty.as_ref()) {
                    out.push_new(HirKind::Builtin { op: Builtin::Cow { unique: true }, args: vec![moved] }, ty.clone(), span)
                } else {
                    moved
                };
                stmts.push(out.push_new(HirKind::Let { local: t, init: Some(init) }, Some(ty::unit()), span));
                let x = &mut out.exprs[root.0 as usize];
                x.kind = HirKind::Local(t);
                x.node = None;
            }
        }
        for &step in &chain[held.clamp(1, chain.len())..] {
            if cow_ty(checker, out.expr(step).ty.as_ref()) && !borrow_field(checker, out, step) {
                stmts.push(unique_stmt(out, step, span));
            }
        }
        stmts
    }
}

/// Whether `place` is a view's `mut` field: the object it borrows was made
/// unique when the view was built, and nothing shares it while the view
/// lives (E0149), so a write through it needs no copy check.
fn borrow_field(checker: &Checker, body: &HirBody, place: HirId) -> bool {
    let HirKind::Field { base, name } = &body.expr(place).kind else {
        return false;
    };
    crate::hir::view_lint::view_key(checker, body.expr(*base).ty.as_ref()).is_some_and(|key| checker.is_borrow_field(&key, name))
}

/// `place = unique(place)` for a place made of locals, fields, elements
/// and pure indexes.
fn unique_stmt(out: &mut HirBody, place: HirId, span: Span) -> HirId {
    let target = dup(out, place);
    let read = dup(out, place);
    let ty = out.expr(place).ty.clone();
    let layout = out.expr(place).layout.clone();
    let u = out.push_new(HirKind::Builtin { op: Builtin::Cow { unique: true }, args: vec![read] }, ty, span);
    out.exprs[u.0 as usize].layout = layout;
    out.push_new(HirKind::Assign { place: target, value: u }, Some(ty::unit()), span)
}

/// A copy of the subtree at `id` with new nodes (a place: locals, fields,
/// elements, pure indexes).
fn dup(out: &mut HirBody, id: HirId) -> HirId {
    let mut e = out.expr(id).clone();
    e.kind = match e.kind {
        HirKind::Field { base, name } => HirKind::Field { base: dup(out, base), name },
        HirKind::Index { base, index, kind } => HirKind::Index { base: dup(out, base), index: dup(out, index), kind },
        HirKind::Bin { op, lhs, rhs } => HirKind::Bin { op, lhs: dup(out, lhs), rhs: dup(out, rhs) },
        HirKind::Un { op, operand } => HirKind::Un { op, operand: dup(out, operand) },
        HirKind::Cast { value } => HirKind::Cast { value: dup(out, value) },
        k => k,
    };
    e.node = None;
    e.flags = HirFlags::default();
    let new = HirId(out.exprs.len() as u32);
    out.exprs.push(e);
    new
}

fn share_local(out: &mut HirBody, l: LocalId, span: Span) -> HirId {
    let ty = out.local(l).ty.clone();
    let read = out.push_new(HirKind::Local(l), ty.clone(), span);
    out.push_new(HirKind::Builtin { op: Builtin::Cow { unique: false }, args: vec![read] }, ty, span)
}

/// Statements to run before others, gathered per place: `(block, at)`
/// inserts into `block` before its statement or tail `at`; `(at, at)`
/// wraps `at` itself in a block.
#[derive(Default)]
struct Before {
    keys: Vec<(HirId, HirId)>,
    stmts: HashMap<(HirId, HirId), Vec<HirId>>,
}

impl Before {
    fn add(&mut self, at: (HirId, HirId), stmt: HirId) {
        let list = self.stmts.entry(at).or_default();
        if list.is_empty() {
            self.keys.push(at);
        }
        list.push(stmt);
    }

    fn apply(self, out: &mut HirBody) {
        for key in &self.keys {
            let new = &self.stmts[key];
            let (block, at) = *key;
            if block == at {
                prepend(out, at, new.clone());
                continue;
            }
            if let HirKind::Block { stmts, tail } = &mut out.exprs[block.0 as usize].kind {
                match stmts.iter().position(|s| *s == at) {
                    Some(i) => {
                        stmts.splice(i..i, new.iter().copied());
                    }
                    None if *tail == Some(at) => stmts.extend(new.iter().copied()),
                    None => {}
                }
            }
        }
    }
}

/// Run `stmts` before node `at`: `at` becomes `{ stmts; at }`.
fn prepend(out: &mut HirBody, at: HirId, mut stmts: Vec<HirId>) {
    let ty = out.expr(at).ty.clone();
    let layout = out.expr(at).layout.clone();
    let inner = relocate(out, at);
    let unit = ty.as_ref().is_none_or(|t| *t == ty::unit());
    let tail = if unit && matches!(out.expr(inner).kind, HirKind::Assign { .. } | HirKind::Append { .. }) {
        stmts.push(inner);
        None
    } else {
        Some(inner)
    };
    let x = &mut out.exprs[at.0 as usize];
    x.kind = HirKind::Block { stmts, tail };
    x.node = None;
    x.layout = layout;
}

/// Move node `id` to a new node and return it, so `id` can be rewritten
/// while its parent still points at it.
fn relocate(out: &mut HirBody, id: HirId) -> HirId {
    let moved = out.exprs[id.0 as usize].clone();
    let new = HirId(out.exprs.len() as u32);
    out.exprs.push(moved);
    new
}
