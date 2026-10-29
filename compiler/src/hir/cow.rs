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
//! it in place, and stores of the parameter itself copy it eagerly. Class
//! instances whose type has a `drop` are identities the VM never copies; a
//! `readonly class` is never written, so it needs neither op.
//!
//! The rewrite runs once per module, after the effects fixed point and
//! before typed inlining, so spliced callees carry their ops.

use std::collections::{HashMap, HashSet};

use super::escape::CallLookup;
use super::{is_vec, pat_binds, Summary, VEC_MUTATORS};
use crate::hir::lower::{children, pure_index};
use crate::hir::{Builtin, Callee, HirBody, HirExpr, HirFlags, HirId, HirKind, HirLocal, HirModule, HirPat, IndexKind, LocalId, LocalKind, MakeKind, Span};
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
/// function, a record, a `readonly class` or a class with a `drop`. A type
/// parameter counts, since it may stand for one.
pub(super) fn cow_ty(checker: &Checker, ty: Option<&Ty>) -> bool {
    let Some(ty) = ty else { return false };
    let name = match strip_readonly(ty) {
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
    if matches!(name, "Range" | "RangeInclusive") {
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
        return !checker.is_readonly_class(&key) && !checker.is_shared_class(&key) && !checker.class_has_drop(name);
    }
    checker.enum_variants(name).is_none()
}

/// What a call does with each argument (bit `i` for argument `i`).
#[derive(Debug, Clone, Copy, Default)]
struct ArgUse {
    /// The callee may write through the argument.
    written: u64,
    /// The callee may keep the argument past the call.
    kept: u64,
    /// Arguments that reach code not known here: code that shares what it
    /// stores (so `kept` is clear), but a caller's fresh object passed
    /// there may be kept all the same.
    opaque: u64,
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
    /// Parameters the body assigns.
    reassigned: HashSet<LocalId>,
    /// Assignments to a `use (mut x)` capture (#2072).
    capture_assigns: Vec<HirId>,
}

/// The body with its copy-on-write ops, when it needs any, and warnings
/// for writes that copy or are dropped.
pub(super) fn rewrite(checker: &Checker, module: &HirModule, body: &HirBody, lookup: &CallLookup<'_>, mutates: u64) -> (Option<HirBody>, Vec<Message>) {
    let Some(root) = body.root else { return (None, Vec::new()) };
    let facts = Facts::new(checker, module, body, lookup, root);
    let mut plan = Plan::new(checker, body, &facts, mutates);
    let notes = std::mem::take(&mut plan.notes);
    if plan.is_empty() {
        return (None, notes);
    }
    let mut out = body.clone();
    plan.apply(&mut out, checker);
    (Some(out), notes)
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
    fn new(checker: &Checker, module: &HirModule, body: &'b HirBody, lookup: &CallLookup<'_>, root: HirId) -> Self {
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
            defer_captured: HashSet::new(),
            reassigned: HashSet::new(),
            capture_assigns: Vec::new(),
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
            let kids = children(body, id);
            for &k in kids.iter().rev() {
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
            HirKind::Call { callee, args } => {
                let used = arg_use(body, lookup, callee, args);
                if std::env::var("COIL_COW_TRACE").is_ok() {
                    eprintln!("cow:   {callee:?} written={:b} kept={:b} summary={:?}", used.written, used.kept, lookup(callee, args).map(|s| (s.mutates, s.keeps, s.returns)));
                }
                for (i, &a) in args.iter().enumerate().take(64) {
                    let a = named_value(body, a);
                    if used.written & (1 << i) != 0 && cow_ty(checker, body.expr(a).ty.as_ref()) {
                        self.writes.push(Write { at: id, path: a });
                    }
                }
                if lookup(callee, args).is_some_and(|s| !s.shared_ret) {
                    self.fresh_calls.insert(id);
                }
                self.calls.insert(id, used);
            }
            HirKind::Match { scrutinee, arms } => {
                for arm in arms {
                    let mut ls = Vec::new();
                    pat_binds(&arm.pat, &mut ls);
                    for l in ls {
                        self.bound.insert(l, (*scrutinee, arm.body));
                    }
                }
            }
            HirKind::ForIn { pat, iterable, kind, .. } => {
                if matches!(kind, Some(ForInKind::Array)) {
                    let mut ls = Vec::new();
                    pat_binds(pat, &mut ls);
                    for l in ls {
                        self.bound.insert(l, (*iterable, id));
                    }
                }
            }
            HirKind::LetPat { pat, init } => {
                let mut ls = Vec::new();
                pat_binds(pat, &mut ls);
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
/// Second: the parameters it hands, as themselves, to code not known
/// here, which may keep them.
pub(super) fn keeps(module: &HirModule, body: &HirBody, lookup: &CallLookup<'_>) -> (u64, u64) {
    if body.is_coro {
        return (u64::MAX, u64::MAX);
    }
    let Some(root) = body.root else { return (0, 0) };
    // What each local may hold of the parameters, through `let x = p`.
    let mut holds: HashMap<LocalId, u64> = HashMap::new();
    for (i, &p) in body.params.iter().enumerate().take(64) {
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
    let mut opaque = 0u64;
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
                let used = arg_use(body, lookup, callee, args);
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
            HirKind::Return(Some(v)) | HirKind::Yield { value: v, .. } | HirKind::Resume { value: Some(v), .. } => kept |= of(&holds, *v),
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
            kept |= of(&holds, v);
        }
    }
    (kept, opaque)
}

/// What the callee of `callee(args)` does with each argument.
fn arg_use(body: &HirBody, lookup: &CallLookup<'_>, callee: &Callee, args: &[HirId]) -> ArgUse {
    let all = if args.len() >= 64 { u64::MAX } else { (1u64 << args.len()) - 1 };
    let unknown = ArgUse { written: all, kept: 0, opaque: all };
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
        return ArgUse { written: any(s.mutates), kept: any(s.keeps), opaque: any(s.opaque_args) };
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
fn from_summary(s: &Summary) -> ArgUse {
    ArgUse { written: s.mutates, kept: s.keeps, opaque: s.opaque_args }
}

/// Where the rewrite puts ops.
#[derive(Default)]
struct Plan {
    /// Warnings about copies and dropped writes.
    notes: Vec<Message>,
    /// Values to share: (node, eager copy).
    share: Vec<(HirId, bool)>,
    /// Locals to share as a statement right after the node that binds
    /// them (pattern bindings) or before a node (captures).
    share_after: Vec<(HirId, LocalId)>,
    share_before: Vec<((HirId, HirId), LocalId)>,
    /// Locals to make unique as a statement before a node (borrowed
    /// captures).
    unique_before: Vec<((HirId, HirId), LocalId)>,
    /// Write paths to make unique before their node.
    unique: Vec<(Write, (HirId, HirId))>,
    /// Locals never shared and only ever given fresh objects.
    fresh_locals: HashSet<LocalId>,
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
        let inout: HashSet<LocalId> = body
            .params
            .iter()
            .enumerate()
            .filter(|(i, l)| *i < 64 && mutates & (1 << i) != 0 && !f.reassigned.contains(l) && !f.captured.contains(l))
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
                        plan.store(body, f, named_value(body, a), &cow, &inout, &borrowed);
                    }
                }
                HirKind::Call { args, .. } => {
                    let kept = f.calls.get(&id).map_or(0, |u| u.kept);
                    for (j, &a) in args.iter().enumerate().take(64) {
                        if kept & (1 << j) != 0 {
                            plan.store(body, f, named_value(body, a), &cow, &inout, &borrowed);
                        }
                    }
                }
                HirKind::Return(Some(v)) | HirKind::Yield { value: v, .. } | HirKind::Resume { value: Some(v), .. } => {
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
                    plan.store(body, f, v, &cow, &inout, &borrowed);
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
                    .filter(move |(j, _)| (u.opaque | u.kept) & (1 << j) != 0)
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

        // Writes: make each path unique top-down.
        for &w in &f.writes {
            if let Some(HirKind::Local(l)) = f.root_of(w.path).map(|r| &body.expr(r).kind)
                && inout.contains(l)
                && w.path == f.root_of(w.path).unwrap_or(w.path)
            {
                // The parameter itself, written in place for the caller.
                continue;
            }
            plan.unique.push((w, f.statement_of(w.at)));
        }
        plan.inout = inout;
        plan.defer_captured = f.defer_captured.clone();
        plan.note_copies(checker, body, f, &copied_from);
        plan
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
            if name.starts_with('$') {
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
                    if !borrowed.contains(l) && f.last_read(*l, at) && owned(f, body, *l, inout) {
                        continue;
                    }
                    self.share_value(body, f, at, inout);
                }
                HirKind::Field { .. } | HirKind::Index { .. } | HirKind::Global { .. } if cow(at) => self.share_value(body, f, at, inout),
                _ => {}
            }
        }
    }
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
            work.extend(children(body, n));
        }
    }
    out
}

/// Whether code emission packs `id` as an existential (by node or span).
fn pack_site(checker: &Checker, body: &HirBody, id: HirId) -> bool {
    let e = body.expr(id);
    e.node.is_some_and(|n| checker.existential_pack_at(n).is_some()) || checker.existential_pack_span(e.span.0, e.span.1).is_some()
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
            for s in self.unique_path(out, checker, w.path, w.at) {
                before.add(at, s);
            }
        }
        before.apply(out);
        // Pattern-bound and captured locals: a share statement.
        for &(binder, l) in &self.share_after {
            let span = span_of(out, binder);
            let stmt = share_local(out, l, span);
            match out.expr(binder).kind.clone() {
                HirKind::ForIn { pat, iterable, body: b, kind } => {
                    let inner = relocate(out, b);
                    out.exprs[b.0 as usize].kind = HirKind::Block { stmts: vec![stmt], tail: Some(inner) };
                    out.exprs[b.0 as usize].node = None;
                    let _ = (pat, iterable, kind);
                }
                HirKind::LetPat { .. } => {
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
            let read = push(out, HirKind::Local(l), ty, span);
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
    /// writing each copy back where it was read.
    fn unique_path(&self, out: &mut HirBody, checker: &Checker, path: HirId, stmt: HirId) -> Vec<HirId> {
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
            let t = temp(out, out.expr(value).ty.clone(), "$cowv");
            let moved = relocate(out, value);
            stmts.push(push(out, HirKind::Let { local: t, init: Some(moved) }, Some(ty::unit()), span));
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
                let t = temp(out, out.expr(index).ty.clone(), "$cowi");
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
                stmts.push(push(out, HirKind::Let { local: t, init: Some(moved) }, Some(ty::unit()), span));
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
                let skip = self.fresh_locals.contains(&l) || self.inout.contains(&l) || self.defer_captured.contains(&l) || !cow_ty(checker, out.local(l).ty.as_ref());
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
                let t = temp(out, ty.clone(), "$cowt");
                let moved = relocate(out, root);
                let init = if cow_ty(checker, ty.as_ref()) {
                    push(out, HirKind::Builtin { op: Builtin::Cow { unique: true }, args: vec![moved] }, ty.clone(), span)
                } else {
                    moved
                };
                stmts.push(push(out, HirKind::Let { local: t, init: Some(init) }, Some(ty::unit()), span));
                let x = &mut out.exprs[root.0 as usize];
                x.kind = HirKind::Local(t);
                x.node = None;
            }
        }
        for &step in &chain[1..] {
            if cow_ty(checker, out.expr(step).ty.as_ref()) {
                stmts.push(unique_stmt(out, step, span));
            }
        }
        stmts
    }
}

/// `place = unique(place)` for a place made of locals, fields, elements
/// and pure indexes.
fn unique_stmt(out: &mut HirBody, place: HirId, span: Span) -> HirId {
    let target = dup(out, place);
    let read = dup(out, place);
    let ty = out.expr(place).ty.clone();
    let layout = out.expr(place).layout.clone();
    let u = push(out, HirKind::Builtin { op: Builtin::Cow { unique: true }, args: vec![read] }, ty, span);
    out.exprs[u.0 as usize].layout = layout;
    push(out, HirKind::Assign { place: target, value: u }, Some(ty::unit()), span)
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
    let read = push(out, HirKind::Local(l), ty.clone(), span);
    push(out, HirKind::Builtin { op: Builtin::Cow { unique: false }, args: vec![read] }, ty, span)
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

fn push(out: &mut HirBody, kind: HirKind, ty: Option<Ty>, span: Span) -> HirId {
    let id = HirId(out.exprs.len() as u32);
    out.exprs.push(HirExpr {
        kind,
        ty,
        layout: crate::layout::Layout::Word,
        span,
        node: None,
        flags: HirFlags::default(),
    });
    id
}

fn temp(out: &mut HirBody, ty: Option<Ty>, prefix: &str) -> LocalId {
    let id = LocalId(out.locals.len() as u32);
    out.locals.push(HirLocal {
        name: format!("{prefix}{}", id.0),
        ty,
        kind: LocalKind::Temp,
        captured: false,
        borrow: false,
    });
    id
}

/// Move node `id` to a new node and return it, so `id` can be rewritten
/// while its parent still points at it.
fn relocate(out: &mut HirBody, id: HirId) -> HirId {
    let moved = out.exprs[id.0 as usize].clone();
    let new = HirId(out.exprs.len() as u32);
    out.exprs.push(moved);
    new
}

#[allow(dead_code)]
fn pat_locals(pat: &HirPat) -> Vec<LocalId> {
    let mut out = Vec::new();
    pat_binds(pat, &mut out);
    out
}
