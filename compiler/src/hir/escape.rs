//! Escape facts per body (`feature/escape-memory` step 1): which
//! parameters a call may keep, which it may hand back in its result, and
//! whether its result is fresh.
//!
//! A value *escapes* when something may still reach it after the call
//! returns other than through the result: a store into an object that is
//! not a fresh object of this body, a static, a closure or `defer` capture,
//! a `yield`, or an argument a callee keeps. A parameter is *returned* when
//! the result may reach it. A result is *fresh* when everything it reaches
//! was allocated by this call (here or in a callee with a fresh result) and
//! nothing else reaches it: a caller that drops the result can free the
//! whole graph.
//!
//! The facts ride on [`Summary`] (`escapes`, `returns`, `shared_ret`), so
//! the effects fixed point solves them across calls and modules. Every
//! fact starts optimistic and only grows, so the loop ends; a callee the
//! analysis cannot see keeps everything and returns nothing fresh.
//!
//! The walk is flow-insensitive: a local holds every value ever assigned
//! to it, and a value derived from another (a field, an element, a
//! pattern binding) may reach whatever its source reaches.

use std::collections::HashMap;

use super::{Builtin, BinOp, Callee, Cx, HirBody, HirId, HirKind, HirModule, HirPat, LocalId, LocalKind, MakeKind, Summary, Walk};
use super::{is_vec, pat_binds, VEC_MUTATORS};
use crate::typechecking::ty::Ty;

/// Bit `i` of [`Src::sites`] for the first 63 allocation sites; later
/// sites share bit 63, which only merges their facts.
const SITE_BITS: u32 = 64;

/// What a value may reach.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Src {
    /// Parameters (bit `i` for parameter `i`, below 64).
    pub(super) params: u64,
    /// Objects allocated in this body, by site.
    pub(super) sites: u64,
    /// Anything else: a static, a capture, a callee's
    /// result that is not fresh.
    pub(super) other: bool,
}

impl Src {
    pub(super) const EMPTY: Self = Self { params: 0, sites: 0, other: false };
    const OTHER: Self = Self { params: 0, sites: 0, other: true };

    pub(super) fn union(self, o: Self) -> Self {
        Self {
            params: self.params | o.params,
            sites: self.sites | o.sites,
            other: self.other || o.other,
        }
    }

    fn is_empty(self) -> bool {
        self == Self::EMPTY
    }
}

/// Leaf objects: a string, or a vector of plain words (packed bytes,
/// ints). They hold no references, so a new one never holds an old one.
pub(super) fn leaf(ty: Option<&Ty>) -> bool {
    match ty.map(crate::typechecking::ty::strip_readonly) {
        Some(Ty::Con(n)) => n == "string",
        Some(t @ Ty::App(_, args)) => is_vec(Some(t)) && args.len() == 1 && no_heap(Some(&args[0])),
        _ => false,
    }
}

/// Whether a value of `ty` is a word with nothing behind it.
pub(super) fn no_heap(ty: Option<&Ty>) -> bool {
    match ty {
        Some(Ty::Readonly(inner)) => no_heap(Some(inner)),
        Some(Ty::Con(n)) => matches!(
            n.as_str(),
            "int" | "float" | "bool" | "byte" | "unit" | "char" | "f32" | "f64" | "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64"
        ),
        _ => false,
    }
}

/// The escape facts of body `w.index`, given the summaries of this
/// iteration of the fixed point: `(escapes, returns, shared_ret)`.
pub(super) fn summarize(cx: &Cx<'_>, w: &Walk<'_>) -> (u64, u64, bool) {
    let body = &cx.module.bodies[w.index];
    let lookup = |callee: &Callee, args: &[HirId]| match callee {
        Callee::Named { name, def, .. } => Some(cx.named(w, name, *def)),
        Callee::Method { name } => cx.method(w, *args.first()?, name).map(|(s, _)| s),
        Callee::Value(f) => cx.fn_value(w, *f),
    };
    let mut e = Escape::new(cx.module, body, &lookup);
    e.solve_locals();
    let mut escaped = Src::EMPTY;
    let mut returned = Src::EMPTY;
    // A coroutine's frame outlives the call that starts it.
    if body.is_coro {
        escaped.params = u64::MAX;
    }
    for i in 0..body.exprs.len() {
        e.sink(HirId(i as u32), &mut escaped, &mut returned);
    }
    if let Some(root) = body.root {
        for v in result_exprs(body, root) {
            returned = returned.union(e.src(v));
        }
    }
    let escaped = e.close(escaped);
    let returned = e.close(returned);
    let shared_ret = returned.other || returned.params != 0 || returned.sites & escaped.sites != 0;
    (escaped.params, returned.params & !escaped.params, shared_ret)
}

/// The expressions whose values a body's root hands back without
/// `return`: the root, through blocks, `if` and `match` tails.
pub(super) fn result_exprs(body: &HirBody, root: HirId) -> Vec<HirId> {
    let mut out = Vec::new();
    let mut work = vec![root];
    while let Some(id) = work.pop() {
        match &body.expr(id).kind {
            HirKind::Block { tail: Some(t), .. } => work.push(*t),
            HirKind::Block { tail: None, .. } => {}
            HirKind::If { then, els, .. } => {
                work.push(*then);
                work.extend(*els);
            }
            HirKind::Match { arms, .. } => work.extend(arms.iter().map(|a| a.body)),
            _ => out.push(id),
        }
    }
    out
}

/// The summary of a call's callee, by callee and arguments; `None` when
/// it is unknown.
pub(super) type CallLookup<'c> = dyn Fn(&Callee, &[HirId]) -> Option<Summary> + 'c;

pub(super) struct Escape<'c, 'a> {
    module: &'a HirModule,
    lookup: &'c CallLookup<'c>,
    body: &'a HirBody,
    /// Allocation site → bit.
    sites: HashMap<HirId, u32>,
    /// What each local may hold.
    locals: HashMap<LocalId, Src>,
    /// What each fresh object of this body may hold (stores into it).
    contained: HashMap<u32, Src>,
}

impl<'c, 'a> Escape<'c, 'a> {
    pub(super) fn new(module: &'a HirModule, body: &'a HirBody, lookup: &'c CallLookup<'c>) -> Self {
        Self {
            module,
            lookup,
            body,
            sites: HashMap::new(),
            locals: HashMap::new(),
            contained: HashMap::new(),
        }
    }

    fn site(&mut self, id: HirId) -> Src {
        let n = self.sites.len() as u32;
        let bit = *self.sites.entry(id).or_insert(n.min(SITE_BITS - 1));
        Src {
            sites: 1 << bit,
            ..Src::EMPTY
        }
    }

    /// `s` and everything the fresh objects in it may hold.
    pub(super) fn close(&self, mut s: Src) -> Src {
        loop {
            let mut next = s;
            for bit in 0..SITE_BITS {
                if s.sites & (1 << bit) != 0
                    && let Some(c) = self.contained.get(&bit)
                {
                    next = next.union(*c);
                }
            }
            if next == s {
                return s;
            }
            s = next;
        }
    }

    /// Everything stored into fresh objects of this body.
    pub(super) fn held(&self) -> Src {
        let all = self.contained.values().fold(Src::EMPTY, |m, c| m.union(*c));
        self.close(all)
    }

    pub(super) fn local(&self, l: LocalId) -> Src {
        self.locals.get(&l).copied().unwrap_or(Src::EMPTY)
    }

    /// Fill [`Self::locals`] to a fixed point: parameters, then every
    /// `let`, assignment, pattern binding and `for` variable, and the
    /// stores into fresh objects.
    pub(super) fn solve_locals(&mut self) {
        let body = self.body;
        for (i, p) in body.params.iter().enumerate() {
            let s = if i < 64 {
                Src {
                    params: 1 << i,
                    ..Src::EMPTY
                }
            } else {
                Src::OTHER
            };
            self.locals.insert(*p, s);
        }
        for (i, l) in body.locals.iter().enumerate() {
            if l.kind == LocalKind::Capture {
                self.locals.insert(LocalId(i as u32), Src::OTHER);
            }
        }
        for (_, inner) in &body.captures {
            self.locals.insert(*inner, Src::OTHER);
        }
        loop {
            let before = (self.locals.clone(), self.contained.clone());
            for i in 0..body.exprs.len() {
                let id = HirId(i as u32);
                match &body.expr(id).kind {
                    HirKind::Let { local, init: Some(init) } => {
                        let s = self.src(*init);
                        self.add_local(*local, s);
                    }
                    HirKind::LetPat { pat, init } => {
                        let s = self.src(*init);
                        self.bind(pat, s);
                    }
                    HirKind::Assign { place, value } => {
                        let s = self.src(*value);
                        match body.expr(*place).kind {
                            HirKind::Local(l) => self.add_local(l, s),
                            HirKind::Field { base, .. } | HirKind::Index { base, .. } => self.store(base, s),
                            _ => {}
                        }
                    }
                    HirKind::Append { base, value } => {
                        let s = self.src(*value);
                        self.store(*base, s);
                    }
                    HirKind::Match { scrutinee, arms } => {
                        let s = self.src(*scrutinee);
                        for arm in arms {
                            self.bind(&arm.pat, s);
                        }
                    }
                    HirKind::ForIn { pat, iterable, .. } => {
                        let s = self.src(*iterable);
                        self.bind(pat, s);
                    }
                    HirKind::Call { callee: Callee::Method { name }, args }
                        if VEC_MUTATORS.contains(&name.as_str())
                            && args.first().is_some_and(|r| is_vec(body.expr(*r).ty.as_ref())) =>
                    {
                        let s = args[1..].iter().fold(Src::EMPTY, |m, a| m.union(self.src(*a)));
                        self.store(args[0], s);
                    }
                    // A callee that changes what an argument reaches may
                    // store anything there, so a fresh object passed to it
                    // may come back holding what this body cannot see.
                    HirKind::Call { callee, args } => {
                        let s = self.callee(id, callee, args);
                        for (j, a) in args.iter().enumerate() {
                            // A leaf (a string, a vector of plain words)
                            // can only be given plain words.
                            if s.is_none_or(|s| j >= 64 || s.mutates & (1 << j) != 0) && !leaf(body.expr(*a).ty.as_ref()) {
                                self.store(*a, Src::OTHER);
                            }
                        }
                    }
                    _ => {}
                }
            }
            if (self.locals.clone(), self.contained.clone()) == before {
                return;
            }
        }
    }

    fn add_local(&mut self, l: LocalId, s: Src) {
        if s.is_empty() {
            return;
        }
        let e = self.locals.entry(l).or_default();
        *e = e.union(s);
    }

    fn bind(&mut self, pat: &HirPat, s: Src) {
        let mut binds = Vec::new();
        pat_binds(pat, &mut binds);
        for l in binds {
            if !no_heap(self.body.local(l).ty.as_ref()) {
                self.add_local(l, s);
            }
        }
    }

    /// `s` is stored into an object reached by `base`. Into a fresh
    /// object of this body it is held there; anywhere else it escapes,
    /// which [`Self::sink`] records.
    fn store(&mut self, base: HirId, s: Src) {
        let b = self.src(base);
        if b.params == 0 && !b.other {
            for bit in 0..SITE_BITS {
                if b.sites & (1 << bit) != 0 {
                    let e = self.contained.entry(bit).or_default();
                    *e = e.union(s);
                }
            }
        }
    }

    /// Whether a store into what `base` reaches keeps the value inside
    /// fresh objects of this body.
    fn stores_into_fresh(&mut self, base: HirId) -> bool {
        let b = self.src(base);
        b.params == 0 && !b.other && b.sites != 0
    }

    /// The summary of a call's callee, when its arguments map to its
    /// parameters by position.
    pub(super) fn callee(&self, at: HirId, callee: &Callee, args: &[HirId]) -> Option<Summary> {
        let body = self.body;
        // A call that yields a function may be a partial application,
        // whose closure keeps the arguments given.
        if matches!(body.expr(at).ty.as_ref().map(crate::typechecking::ty::strip_readonly), Some(Ty::Fun(..))) {
            return None;
        }
        let positional = args
            .iter()
            .all(|a| !matches!(body.expr(*a).kind, HirKind::Named { .. } | HirKind::Spread(_)));
        if !positional {
            return None;
        }
        if let Callee::Method { name } = callee
            && let Some(recv) = args.first()
            && let recv_ty = body.expr(*recv).ty.as_ref()
            && (is_vec(recv_ty) && matches!(name.as_str(), "len" | "capacity")
                || name == "len" && args.len() == 1 && recv_ty.is_some_and(crate::typechecking::infer::Checker::is_structural_len_ty_for_codegen))
        {
            return Some(Summary::default());
        }
        (self.lookup)(callee, args)
    }

    /// What the value of `id` may reach.
    pub(super) fn src(&mut self, id: HirId) -> Src {
        let body = self.body;
        let e = body.expr(id);
        if no_heap(e.ty.as_ref()) {
            return Src::EMPTY;
        }
        match &e.kind {
            // A string literal is interned: immutable, and never freed early
            // (the VM skips interned strings), so holding one is like
            // holding nothing.
            HirKind::Lit(_) => Src::EMPTY,
            HirKind::Local(l) => self.local(*l),
            HirKind::Global { .. } => Src::OTHER,
            HirKind::Field { base, .. } | HirKind::Index { base, .. } => {
                let b = self.src(*base);
                self.close(b)
            }
            HirKind::Bin { op: BinOp::StrConcat, .. } => self.site(id),
            HirKind::Bin { .. } | HirKind::Logic { .. } | HirKind::Un { .. } => Src::OTHER,
            HirKind::Cast { value } | HirKind::Named { value, .. } | HirKind::Spread(value) => self.src(*value),
            HirKind::Call { callee, args } => {
                if let Callee::Method { name } = callee
                    && VEC_MUTATORS.contains(&name.as_str())
                    && args.first().is_some_and(|r| is_vec(body.expr(*r).ty.as_ref()))
                {
                    // `pop` / `remove` hand back an element.
                    let b = self.src(args[0]);
                    return self.close(b);
                }
                let Some(s) = self.callee(id, callee, args) else {
                    return Src::OTHER;
                };
                let mut out = if s.shared_ret { Src::OTHER } else { self.site(id) };
                for (j, a) in args.iter().enumerate() {
                    if j >= 64 || s.returns & (1 << j) != 0 {
                        out = out.union(self.src(*a));
                    }
                }
                out
            }
            HirKind::Make { kind, args } => {
                let mut out = self.site(id);
                if !matches!(kind, MakeKind::Range { .. }) {
                    for a in args {
                        out = out.union(self.src(*a));
                    }
                }
                out
            }
            HirKind::Block { tail, .. } => tail.map_or(Src::EMPTY, |t| self.src(t)),
            HirKind::If { then, els, .. } => {
                let t = self.src(*then);
                els.map_or(t, |x| t.union(self.src(x)))
            }
            HirKind::Match { arms, .. } => arms.iter().fold(Src::EMPTY, |m, a| m.union(self.src(a.body))),
            HirKind::Lambda { body: inner } => {
                let mut out = self.site(id);
                for (outer, _) in &self.module.bodies[*inner].captures {
                    out = out.union(self.local(*outer));
                }
                out
            }
            HirKind::Builtin { op, args } => match op {
                Builtin::Box { .. } => args.iter().fold(self.site(id), |m, a| m.union(self.src(*a))),
                Builtin::Unbox { .. } | Builtin::Relayout => args.first().map_or(Src::EMPTY, |a| self.src(*a)),
                Builtin::Stringify => self.site(id),
                // The operand's object, or for unique maybe a copy of it.
                Builtin::Cow { unique } => {
                    let s = args.first().map_or(Src::EMPTY, |a| self.src(*a));
                    if *unique { s.union(self.site(id)) } else { s }
                }
                _ => Src::OTHER,
            },
            HirKind::Resume { .. } => Src::OTHER,
            _ => Src::EMPTY,
        }
    }

    /// Record what node `id` lets escape or hands back.
    pub(super) fn sink(&mut self, id: HirId, escaped: &mut Src, returned: &mut Src) {
        let body = self.body;
        let esc = |s: Src, escaped: &mut Src| *escaped = escaped.union(s);
        match &body.expr(id).kind {
            HirKind::Return(Some(v)) => *returned = returned.union(self.src(*v)),
            HirKind::Assign { place, value } => match body.expr(*place).kind {
                HirKind::Local(_) => {}
                HirKind::Field { base, .. } | HirKind::Index { base, .. } if self.stores_into_fresh(base) => {}
                _ => {
                    let s = self.src(*value);
                    esc(s, escaped);
                }
            },
            HirKind::Append { base, value } if !self.stores_into_fresh(*base) => {
                let s = self.src(*value);
                esc(s, escaped);
            }
            HirKind::Yield { value, .. } => {
                let s = self.src(*value);
                esc(s, escaped);
            }
            HirKind::Resume { value: Some(v), .. } => {
                let s = self.src(*v);
                esc(s, escaped);
            }
            HirKind::Lambda { body: inner } => {
                for (outer, _) in &self.module.bodies[*inner].captures {
                    esc(self.local(*outer), escaped);
                }
            }
            HirKind::Defer { captures, .. } => {
                for l in captures.iter().flatten() {
                    esc(self.local(*l), escaped);
                }
            }
            HirKind::Builtin { op, args } => match op {
                Builtin::Box { .. } | Builtin::Unbox { .. } | Builtin::Relayout | Builtin::Stringify | Builtin::Cow { .. } | Builtin::Print | Builtin::Panic | Builtin::TypeOf => {}
                _ => {
                    for a in args {
                        let s = self.src(*a);
                        esc(s, escaped);
                    }
                }
            },
            HirKind::Bin { op: BinOp::Overloaded(_), lhs, rhs } => {
                let s = self.src(*lhs).union(self.src(*rhs));
                esc(s, escaped);
            }
            HirKind::Call { callee, args } => {
                if let Callee::Method { name } = callee
                    && VEC_MUTATORS.contains(&name.as_str())
                    && args.first().is_some_and(|r| is_vec(body.expr(*r).ty.as_ref()))
                {
                    if !self.stores_into_fresh(args[0]) {
                        for a in &args[1..] {
                            let s = self.src(*a);
                            esc(s, escaped);
                        }
                    }
                    return;
                }
                let s = self.callee(id, callee, args);
                for (j, a) in args.iter().enumerate() {
                    let keeps = s.is_none_or(|s| j >= 64 || s.escapes & (1 << j) != 0);
                    if keeps {
                        let v = self.src(*a);
                        esc(v, escaped);
                    }
                }
            }
            _ => {}
        }
    }
}

/// A way an object made inside a `region { … }` block leaves it (E0140).
pub(super) struct RegionLeak {
    /// The region block.
    pub(super) region: crate::hir::Span,
    /// The node the object leaves by.
    pub(super) at: crate::hir::Span,
    pub(super) how: String,
}

/// The objects made inside each `region { … }` block of `body` that can
/// leave it: assigned to a local declared outside, stored into an object
/// from outside or a static, passed where the callee keeps it, captured by
/// a `defer`, yielded, or handed out as the block's value. A `return` (and
/// `?`) out of a region may take an object along: the minor collection at
/// the region's exit copies it. Callees the analysis cannot see are not
/// errors either; the runtime keeps whatever they keep.
pub(super) fn region_leaks(module: &HirModule, body: &HirBody, lookup: &CallLookup<'_>) -> Vec<RegionLeak> {
    // A region lowers to `Block { [enter, inner | let t = inner, exit] }`.
    let regions: Vec<(crate::hir::Span, HirId, bool)> = body
        .exprs
        .iter()
        .filter_map(|x| match &x.kind {
            HirKind::Block { stmts, .. }
                if stmts.len() == 3 && matches!(body.expr(stmts[0]).kind, HirKind::Builtin { op: Builtin::Region { enter: true }, .. }) =>
            {
                match body.expr(stmts[1]).kind {
                    HirKind::Let { init: Some(inner), .. } => Some((x.span, inner, true)),
                    _ => Some((x.span, stmts[1], false)),
                }
            }
            _ => None,
        })
        .collect();
    if regions.is_empty() {
        return Vec::new();
    }
    let mut e = Escape::new(module, body, lookup);
    e.solve_locals();
    for i in 0..body.exprs.len() {
        e.src(HirId(i as u32));
    }
    let mut out = Vec::new();
    for (region, inner, valued) in regions {
        let mut inside = std::collections::HashSet::new();
        let mut work = vec![inner];
        while let Some(at) = work.pop() {
            if inside.insert(at) {
                work.extend(crate::hir::lower::children(body, at));
            }
        }
        let mut declared = std::collections::HashSet::new();
        for &at in &inside {
            match &body.expr(at).kind {
                HirKind::Let { local, .. } => {
                    declared.insert(*local);
                }
                HirKind::LetPat { pat, .. } | HirKind::ForIn { pat, .. } => {
                    let mut binds = Vec::new();
                    pat_binds(pat, &mut binds);
                    declared.extend(binds);
                }
                HirKind::Match { arms, .. } => {
                    for arm in arms {
                        let mut binds = Vec::new();
                        pat_binds(&arm.pat, &mut binds);
                        declared.extend(binds);
                    }
                }
                _ => {}
            }
        }
        // The region's own sites; the last bit is shared, so it never counts.
        let made = e
            .sites
            .iter()
            .filter(|(id, bit)| **bit < SITE_BITS - 1 && inside.contains(id))
            .fold(0u64, |m, (_, bit)| m | (1 << bit));
        if made == 0 {
            continue;
        }
        let leaks = |e: &mut Escape<'_, '_>, value: HirId| {
            let s = e.src(value);
            e.close(s).sites & made != 0
        };
        // Whether `base` may reach anything but this region's objects.
        let outside = |e: &mut Escape<'_, '_>, base: HirId| {
            let b = e.src(base);
            b.params != 0 || b.other || b.sites & !made != 0
        };
        let mut push = |at: HirId, how: String| out.push(RegionLeak { region, at: body.expr(at).span, how });
        let mut ids: Vec<HirId> = inside.iter().copied().collect();
        ids.sort_by_key(|id| id.0);
        for id in ids {
            match &body.expr(id).kind {
                HirKind::Assign { place, value } => match &body.expr(*place).kind {
                    HirKind::Local(l) if declared.contains(l) => {}
                    HirKind::Local(l) => {
                        if leaks(&mut e, *value) {
                            push(id, format!("is assigned to `{}`, declared outside the region", body.local(*l).name));
                        }
                    }
                    HirKind::Field { base, .. } | HirKind::Index { base, .. } => {
                        if outside(&mut e, *base) && leaks(&mut e, *value) {
                            push(id, "is stored into an object from outside the region".into());
                        }
                    }
                    _ => {
                        if leaks(&mut e, *value) {
                            push(id, "is stored into a static".into());
                        }
                    }
                },
                HirKind::Append { base, value } => {
                    if outside(&mut e, *base) && leaks(&mut e, *value) {
                        push(id, "is appended to an object from outside the region".into());
                    }
                }
                HirKind::Yield { value, .. } | HirKind::Resume { value: Some(value), .. } => {
                    if leaks(&mut e, *value) {
                        push(id, "is yielded".into());
                    }
                }
                HirKind::Defer { captures, .. } => {
                    for l in captures.iter().flatten() {
                        let s = e.local(*l);
                        if e.close(s).sites & made != 0 {
                            push(id, format!("is captured by a `defer` (through `{}`)", body.local(*l).name));
                        }
                    }
                }
                HirKind::Call { callee, args } => {
                    if let Callee::Method { name } = callee
                        && VEC_MUTATORS.contains(&name.as_str())
                        && args.first().is_some_and(|r| is_vec(body.expr(*r).ty.as_ref()))
                    {
                        if outside(&mut e, args[0]) && args[1..].iter().any(|a| leaks(&mut e, *a)) {
                            push(id, format!("is added (`{name}`) to a vector from outside the region"));
                        }
                        continue;
                    }
                    let Some(s) = e.callee(id, callee, args) else { continue };
                    for (j, a) in args.iter().enumerate() {
                        if j < 64 && s.escapes & (1 << j) != 0 && leaks(&mut e, *a) {
                            let name = match callee {
                                Callee::Named { name, .. } | Callee::Method { name } => name.rsplit("::").next().unwrap_or(name).to_string(),
                                Callee::Value(_) => "a function value".to_string(),
                            };
                            push(id, format!("is passed to `{name}`, which keeps it"));
                        }
                    }
                }
                _ => {}
            }
        }
        if valued && leaks(&mut e, inner) {
            let tail = match body.expr(inner).kind {
                HirKind::Block { tail: Some(t), .. } => t,
                _ => inner,
            };
            push(tail, "is the region's value".into());
        }
    }
    out
}

/// Host natives that allocate their result and keep nothing they are
/// passed. Early freeing frees what these return, so each is checked
/// against the VM: `to_bytes`, `from_bytes` and `vec_with_capacity`
/// allocate a new object, `string_slice_bytes` a new string (that may
/// share its bytes, which are reference-counted), and `format` (also how
/// `a + b` on strings runs) a new string.
const FRESH_HOSTS: &[&str] = &[
    "to_bytes",
    "from_bytes",
    "string_slice_bytes",
    "slice_bytes",
    "format",
    "vec_with_capacity",
    "with_capacity",
];

/// Host natives with effects that still keep nothing they are passed: the
/// buffer of a `write` is read during the call only.
const NON_RETAINING_HOSTS: &[&str] = &["write", "write_from", "write_all", "read", "print", "println", "eprint", "eprintln"];

/// Whether `name` is in [`FRESH_HOSTS`] or [`NON_RETAINING_HOSTS`].
pub(super) fn is_listed_host(name: &str) -> bool {
    let short = name.rsplit("::").next().unwrap_or(name);
    FRESH_HOSTS.contains(&short) || NON_RETAINING_HOSTS.contains(&short)
}

/// [`Summary::of`] for host native `name`, with its escape facts: a pure
/// native or a known non-retaining one keeps nothing (it may still hand an
/// argument back), and the natives in [`FRESH_HOSTS`] return a fresh object.
pub(super) fn host_summary(name: &str, mut s: Summary) -> Summary {
    let short = name.rsplit("::").next().unwrap_or(name);
    if FRESH_HOSTS.contains(&short) {
        s.escapes = 0;
        s.returns = 0;
        s.shared_ret = false;
    } else if NON_RETAINING_HOSTS.contains(&short)
        || (!s.flags.contains(common::EffectFlags::UNKNOWN) && !s.flags.contains(common::EffectFlags::HEAP_MUT) && !s.flags.contains(common::EffectFlags::HOST))
    {
        s.escapes = 0;
    }
    // Only a host native keeps without sharing; other unknown code shares
    // what it stores (copy-on-write).
    s.keeps = if common::host_native_id(short).is_some() { s.escapes } else { 0 };
    s
}
