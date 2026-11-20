//! Independent Parallel Arms for counted loops (loop IPA).
//!
//! The recursive IPA in [`par_profit`](super::par_profit)
//! treats sibling self-calls as independent arms. A counted loop is the
//! same idea with the arms spread over an induction range: when the
//! iterations only communicate through one associative reduction
//! `acc = acc ⊕ e(i)`, any partition of the range folds to the sequential
//! result.
//!
//! Detection is structural — no function, module or benchmark allowlists —
//! and fails closed. Anything the walk cannot prove independent (a nested
//! loop, an impure call, a second reduction, a non-int capture) leaves the
//! loop sequential. Counted `for x in` / range (Q6 literal, B5 const range
//! locals) share this shape; the latch is implicit.
//!
//! Wide shapes also admit a dynamic int bound, an int parameter or int
//! local capture, a pure `if` whose arms share one reduction, and a
//! positive constant stride.
//!
//! The walk runs on a module's HIR as built, before any pass: a site is
//! keyed by its loop's source span and names its locals, so the plan finds
//! it again in a body the passes rewrote or inlined elsewhere.
//!
//! A site listed here is one the plan can build (codegen's `hir_par_site`
//! and `hir_par_outline`): its accumulator, index, bounds and live captures
//! are `int` locals, few enough for a spawn, and a `while` is the plain
//! test-and-body shape. The plan may still leave a site sequential when
//! chunking gains nothing; a site it cannot build is a bug on one side,
//! which the corpus test reports (`compiler/tests/hir_corpus.rs`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::super::{BinOp, BodyKind, HirBody, HirFlags, HirId, HirKind, HirModule, HirPat, Lit, LocalId, LocalKind, MakeKind, UnOp, query};
use crate::typechecking::infer::Checker;
use super::par_profit::DEFAULT_LOOP_GRAIN;
use super::sign::{Env, Interval, Range, Signs};
use crate::typechecking::ty::{self as coil_ty, Ty};

/// Associative operator folding a loop's per-iteration contributions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopReduceOp {
    Add,
    Mul,
    /// Bitwise xor: associative, commutative, identity `0`.
    Xor,
}

impl LoopReduceOp {
    /// Identity element. Every chunk but the first starts here, so folding the
    /// partials with `⊕` reproduces the sequential `acc`.
    pub fn identity(self) -> i64 {
        match self {
            Self::Add => 0,
            Self::Mul => 1,
            Self::Xor => 0,
        }
    }
}

/// A counted loop whose iterations are independent apart from one reduction.
///
/// The induction range is normalized half-open (`i <= k` becomes `end = k + 1`)
/// so an n-way split is just a partition of `[begin, end)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopParSite {
    /// Induction variable, advanced by exactly one `+ 1` per iteration.
    pub index: String,
    pub begin: i64,
    pub end: i64,
    /// Reduction accumulator: a const-initialized local of an enclosing scope.
    pub acc: String,
    pub op: LoopReduceOp,
    /// Counted `for x in` / range: the body does not contain the `+ 1` step;
    /// the chunk worker emits it after each trip (Q6 latch lives in codegen).
    pub implicit_step: bool,
    /// Enclosing const-int locals the body reads (inlined into the worker).
    pub captures: Vec<(String, i64)>,
    /// Int parameters and non-const int locals passed as extra worker arguments.
    pub live_captures: Vec<String>,
    /// Added to the induction variable each trip. Always positive.
    pub stride: i64,
    /// Runtime start local. `None` means [`Self::begin`] is the start.
    pub begin_local: Option<String>,
    /// Runtime exclusive-end local, before [`Self::end_bias`].
    pub end_local: Option<String>,
    /// Added to a runtime end (`1` for `i <= n` / `..=`). Const ends bake this in.
    pub end_bias: i64,
    /// The tests of loop-invariant locals under which the fold's terms
    /// have one sign ([`SignTest`]), all of which must hold; the
    /// fork-join makes them once, as the loop starts, and runs the loop on
    /// its own thread when one fails. Empty: the terms have one sign
    /// whatever the values.
    pub guard: Vec<SignTest>,
}

/// `local >= bound` (`at_least`) or `local <= bound`, of a live capture or
/// a bound read at run time ([`LoopParSite::guard`]), by its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignTest {
    pub local: String,
    pub at_least: bool,
    pub bound: i64,
}

impl std::fmt::Display for SignTest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} {}", self.local, if self.at_least { ">=" } else { "<=" }, self.bound)
    }
}

/// Upper bound on chunks for one counted loop. The joiner runs the first.
pub const LOOP_PAR_MAX_CHUNKS: i64 = 4;

impl LoopParSite {
    pub fn is_dynamic(&self) -> bool {
        self.begin_local.is_some() || self.end_local.is_some()
    }

    /// Iteration count of a const range. `0` when an endpoint is dynamic.
    pub fn trip_count(&self) -> i64 {
        if self.is_dynamic() {
            return 0;
        }
        iteration_count(self.begin, self.end, self.stride).unwrap_or(0)
    }

    /// 2-way split on the induction lattice. Dynamic sites return `begin`.
    pub fn midpoint(&self) -> i64 {
        let half = self.trip_count() / 2;
        self.begin + half * self.stride
    }

    /// Exclusive chunk edges, including `begin` and `end`.
    ///
    /// `None` for a dynamic range (the split is computed at runtime) or a
    /// trip count that does not clear `grain`. At most [`LOOP_PAR_MAX_CHUNKS`]
    /// pieces; each piece except the last has `trips / n` iterations.
    pub fn chunk_bounds(&self, grain: i64) -> Option<Vec<i64>> {
        if self.is_dynamic() {
            return None;
        }
        let trips = iteration_count(self.begin, self.end, self.stride)?;
        let grain = grain.max(1);
        if trips <= grain {
            return None;
        }
        let n = (trips / grain).clamp(2, LOOP_PAR_MAX_CHUNKS);
        let base = trips / n;
        let mut bounds = Vec::with_capacity(n as usize + 1);
        bounds.push(self.begin);
        for i in 1..n {
            bounds.push(self.begin + i * base * self.stride);
        }
        bounds.push(self.end);
        Some(bounds)
    }

    /// Induction variable after a const loop that actually runs.
    pub fn final_index(&self) -> i64 {
        self.begin + self.stride * self.trip_count()
    }
}

/// Detected sites keyed by the loop node's source span (codegen's join key).
pub type LoopParSites = HashMap<(usize, usize), LoopParSite>;

impl HirModule {
    /// Every counted-loop fork site of the module, whose path is
    /// `module_path`.
    ///
    /// `pure_fns` is the side-effect-free user functions; a loop body may
    /// only call those.
    pub fn loop_par_sites(&self, checker: &Checker, module_path: &str, pure_fns: &HashSet<String>) -> LoopParSites {
        self.loop_par_scan(checker, module_path, pure_fns).0
    }

    /// The loops that are fork sites but for the sign of their fold's
    /// terms ([`Scan::one_signed`]): each runs sequentially.
    pub fn loop_par_unsigned(&self, checker: &Checker, module_path: &str, pure_fns: &HashSet<String>) -> LoopParSites {
        self.loop_par_scan(checker, module_path, pure_fns).1
    }

    /// The sites, and the loops only the sign rule refuses.
    fn loop_par_scan(&self, checker: &Checker, module_path: &str, pure_fns: &HashSet<String>) -> (LoopParSites, LoopParSites) {
        let mut out = LoopParSites::new();
        let mut unsigned = LoopParSites::new();
        let signs = Signs::new(self, checker, module_path);
        for body in &self.bodies {
            let Some(root) = body.root else { continue };
            // The top level holds declarations only; a lambda, a test and
            // a static's initializer run sequentially.
            if !matches!(body.kind, BodyKind::Function | BodyKind::Method) {
                continue;
            }
            let mut ints: IntLocals = body
                .params
                .iter()
                .copied()
                .filter(|&p| matches!(&body.local(p).ty, Some(Ty::Con(n)) if n == coil_ty::INT || n == coil_ty::BYTE))
                .map(|p| (p, Some(Interval::of_type(body.local(p).ty.as_ref()))))
                .collect();
            let mut written = HashSet::new();
            assigned_in(self, body, root, &mut written);
            let mut scan = Scan { module: self, checker, body, pure_fns, signs: &signs, written, out: &mut out, unsigned: &mut unsigned };
            scan.walk(root, &mut ConstLocals::new(), &mut ints);
        }
        (out, unsigned)
    }
}

/// What a local is proven to hold at this point: a compile-time int or a
/// counted range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConstVal {
    Int(i64),
    /// Half-open `[begin, end)`. Inclusive source ranges are normalized here.
    Range {
        begin: i64,
        end: i64,
    },
}

type ConstLocals = HashMap<LocalId, ConstVal>;

/// The locals known to hold an `int` (or a `byte`) at this point, each
/// with the range of what it holds.
type IntLocals = Env;

struct Scan<'a> {
    module: &'a HirModule,
    checker: &'a Checker,
    body: &'a HirBody,
    pure_fns: &'a HashSet<String>,
    signs: &'a Signs<'a>,
    /// The locals something in the body assigns after their `let`.
    written: HashSet<LocalId>,
    out: &'a mut LoopParSites,
    /// The loops only the sign rule refuses.
    unsigned: &'a mut LoopParSites,
}

/// A counted loop's head, before its body is checked.
struct Counted {
    body: HirId,
    index: LocalId,
    begin: i64,
    end: i64,
    begin_local: Option<LocalId>,
    end_local: Option<LocalId>,
    end_bias: i64,
    implicit_step: bool,
    /// The contract checks a `for` runs ahead of its body.
    checks: Vec<HirId>,
}

struct LoopScan<'a> {
    index: LocalId,
    acc: LocalId,
    consts: &'a ConstLocals,
    ints: &'a IntLocals,
    captures: BTreeSet<(String, i64)>,
    /// The live captures, by name.
    live: BTreeMap<String, LocalId>,
}

/// One admissible statement of a loop-IPA body.
enum StmtForm {
    /// `i = i + k` / `i += k` / `i++` with `k > 0`.
    Step(i64),
    /// `acc = acc ⊕ expr` / `acc ⊕= expr`.
    Reduce { acc: LocalId, op: LoopReduceOp, expr: HirId },
    /// `let name = init` — a loop-private temp.
    Local { local: LocalId, init: HirId },
    /// Pure `if` / `else`. Arms share the enclosing reduction.
    If { arms: Vec<IfArm> },
}

struct IfArm {
    cond: Option<HirId>,
    body: Vec<StmtForm>,
}

impl Scan<'_> {
    fn name(&self, l: LocalId) -> &str {
        &self.body.local(l).name
    }

    /// Whether local `l` is an `int`, the one type the fork-join's words
    /// have (`ints` also holds `byte` locals).
    fn is_int(&self, l: LocalId) -> bool {
        self.body.local(l).ty.as_ref().and_then(crate::hir::classify::primitive) == Some(coil_ty::INT)
    }

    /// Statement list: each item may bind or invalidate a const local for
    /// the statements that follow it. `ints` is every local still known to
    /// be an int.
    fn walk_block(&mut self, items: &[HirId], consts: &mut ConstLocals, ints: &mut IntLocals) {
        for &item in items {
            self.walk(item, consts, ints);
            self.note_binding_effects(item, consts, ints);
        }
    }

    /// Walk a loop body that is *not* a fork site.
    ///
    /// Everything the loop assigns is dropped first: a const local's
    /// binding no longer describes every visit to a program point inside a
    /// loop.
    fn walk_loop_body(&mut self, loop_id: HirId, body: HirId, consts: &ConstLocals, ints: &IntLocals) {
        let mut inner = consts.clone();
        let mut inner_ints = ints.clone();
        for l in self.assigned(loop_id) {
            inner.remove(&l);
            inner_ints.remove(&l);
        }
        self.walk(body, &mut inner, &mut inner_ints);
    }

    fn walk(&mut self, id: HirId, consts: &mut ConstLocals, ints: &mut IntLocals) {
        // Walks recurse through here once per level of the tree.
        common::deep_stack(|| self.walk_level(id, consts, ints))
    }

    fn walk_level(&mut self, id: HirId, consts: &mut ConstLocals, ints: &mut IntLocals) {
        let body = self.body;
        let e = body.expr(id);
        match &e.kind {
            // A nested block's own bindings do not outlive it.
            HirKind::Block { stmts, tail } => {
                let items: Vec<HirId> = stmts.iter().copied().chain(*tail).collect();
                self.walk_block(&items, &mut consts.clone(), &mut ints.clone());
            }
            HirKind::Let { init: Some(init), .. } => self.walk(*init, &mut consts.clone(), &mut ints.clone()),
            HirKind::Return(Some(value)) => self.walk(*value, consts, ints),
            HirKind::If { cond, then, els } => {
                let mut on = ints.clone();
                self.assume(*cond, true, &mut on);
                self.walk(*then, &mut consts.clone(), &mut on);
                if let Some(els) = els {
                    let mut off = ints.clone();
                    self.assume(*cond, false, &mut off);
                    self.walk(*els, &mut consts.clone(), &mut off);
                }
            }
            HirKind::Match { arms, .. } => {
                for arm in arms {
                    self.walk(arm.body, &mut consts.clone(), &mut ints.clone());
                }
            }
            HirKind::Loop { body: loop_body } => {
                // A `while let` is no counted loop; one inside it may be.
                let Some((checks, cond, then)) = query::while_parts(body, *loop_body) else {
                    return self.walk_loop_body(id, *loop_body, consts, ints);
                };
                // A `while` runs its checks ahead of each test, the last
                // one too, and once when it runs no trips: no chunk
                // worker's loop does that, so a checked `while` is no site.
                let site = if checks.is_empty() { self.counted_while(cond, then, consts, ints) } else { None };
                match site.and_then(|c| self.finish(c, consts, ints)) {
                    Some((site, true)) => {
                        self.out.insert(e.span, site);
                    }
                    refused => {
                        if let Some((site, _)) = refused {
                            self.unsigned.insert(e.span, site);
                        }
                        self.walk_loop_body(id, then, consts, ints)
                    }
                }
            }
            // A `for` runs its checks in its body, so a chunk worker runs
            // them as the loop does.
            HirKind::ForIn { pat, iterable, .. } => {
                let Some((checks, own)) = query::for_body_parts(body, id) else { return };
                let site = if self.checks_follow_the_loop(id, checks) { None } else { self.counted_for(pat, *iterable, (checks, own), consts, ints) };
                match site.and_then(|c| self.finish(c, consts, ints)) {
                    Some((site, true)) => {
                        self.out.insert(e.span, site);
                    }
                    refused => {
                        if let Some((site, _)) = refused {
                            self.unsigned.insert(e.span, site);
                        }
                        self.walk_loop_body(id, own, consts, ints)
                    }
                }
            }
            _ => {}
        }
    }

    /// Whether a contract check of `for` loop `loop_id` (`invariant` /
    /// `decreases`) reads a local the loop writes: the accumulator, a
    /// `decreases` clause's own previous value.
    ///
    /// Such a clause is about the state the sequential loop has at that
    /// iteration. A chunk has its own: its fold starts at the identity.
    /// A clause over values the loop leaves alone is the same test in
    /// every iteration of every chunk.
    fn checks_follow_the_loop(&self, loop_id: HirId, checks: &[HirId]) -> bool {
        if checks.is_empty() {
            return false;
        }
        let written = self.assigned(loop_id);
        let mut follows = false;
        for &check in checks {
            self.body.visit(check, &mut |e| match e.kind {
                HirKind::Local(l) => follows |= written.contains(&l),
                // Its body reads through captures this walk does not see.
                HirKind::Lambda { .. } => follows = true,
                _ => {}
            });
        }
        follows
    }

    /// `while i < K { … }` against the loop-IPA shape.
    fn counted_while(&self, cond: HirId, then: HirId, consts: &ConstLocals, ints: &IntLocals) -> Option<Counted> {
        let body = self.body;
        let HirKind::Bin { op, lhs, rhs } = body.expr(cond).kind else { return None };
        let inclusive = match op {
            BinOp::Lt => false,
            BinOp::Le => true,
            _ => return None,
        };
        let index = local_read(body, lhs)?;
        let (end_local, end_const) = int_endpoint(body, rhs, consts, ints)?;
        let (begin, begin_local) = if let Some(ConstVal::Int(b)) = consts.get(&index) {
            (*b, None)
        } else if ints.contains_key(&index) {
            (0, Some(index))
        } else {
            return None;
        };
        let (end, end_bias) = if end_local.is_some() {
            (end_const, i64::from(inclusive))
        } else if inclusive {
            (end_const.checked_add(1)?, 0)
        } else {
            (end_const, 0)
        };
        Some(Counted { body: then, index, begin, end, begin_local, end_local, end_bias, implicit_step: false, checks: Vec::new() })
    }

    /// `for x in START..END` / `..=`, or `for x in r` when `r` is a
    /// const-initialized counted range local (Q6 literal / B5).
    fn counted_for(&self, pat: &HirPat, iterable: HirId, (checks, own): (&[HirId], HirId), consts: &ConstLocals, ints: &IntLocals) -> Option<Counted> {
        let body = self.body;
        let HirPat::Bind(index) = *pat else { return None };
        // `for mut x in` borrows. `for _ in` binds a local of the
        // builder's that no body names: an index like any other.
        if body.local(index).borrow || !matches!(body.local(index).kind, LocalKind::Pattern | LocalKind::Temp) {
            return None;
        }
        let (begin, end, begin_local, end_local, end_bias) = if let Some(ConstVal::Range { begin, end }) = const_val(body, iterable, consts) {
            (begin, end, None, None, 0)
        } else {
            let HirKind::Make { kind: MakeKind::Range { inclusive }, args } = &body.expr(iterable).kind else {
                return None;
            };
            let [start, end] = args.as_slice() else { return None };
            let (begin_local, begin) = int_endpoint(body, *start, consts, ints)?;
            let (end_local, end) = int_endpoint(body, *end, consts, ints)?;
            if begin_local.is_none() && end_local.is_none() {
                let end = if *inclusive { end.checked_add(1)? } else { end };
                (begin, end, None, None, 0)
            } else {
                (begin, end, begin_local, end_local, i64::from(*inclusive))
            }
        };
        Some(Counted { body: own, index, begin, end, begin_local, end_local, end_bias, implicit_step: true, checks: checks.to_vec() })
    }

    /// The site of counted loop `counted`, and whether its fold keeps
    /// one sign ([`Self::one_signed`]): only then is it a site.
    fn finish(&self, counted: Counted, consts: &ConstLocals, ints: &IntLocals) -> Option<(LoopParSite, bool)> {
        let Counted { body: loop_body, index, begin, end, begin_local, end_local, end_bias, implicit_step, checks } = counted;
        let dynamic = begin_local.is_some() || end_local.is_some();
        let forms = self.classify_body(loop_body, index, true)?;
        let mut reduces = Vec::new();
        let mut steps = Vec::new();
        collect_forms(&forms, &mut reduces, &mut steps);
        if steps.len() != usize::from(!implicit_step) {
            return None;
        }
        let stride = if implicit_step { 1 } else { steps[0] };
        if stride <= 0 {
            return None;
        }
        if !dynamic && iteration_count(begin, end, stride)? <= DEFAULT_LOOP_GRAIN {
            return None;
        }
        let (acc, op) = *reduces.first()?;
        if reduces.iter().any(|&(a, o)| a != acc || o != op) || acc == index {
            return None;
        }
        // The accumulator must be a const-initialized local of an enclosing
        // scope: that proves it is a frame slot codegen can find, and that
        // no earlier statement left it with an unknown value.
        if !matches!(consts.get(&acc), Some(ConstVal::Int(_))) {
            return None;
        }
        let mut scan = LoopScan { index, acc, consts, ints, captures: BTreeSet::new(), live: BTreeMap::new() };
        if !self.forms_independent(&forms, &mut HashSet::new(), &mut scan) {
            return None;
        }
        // A chunk worker runs the loop's checks with its body, so they
        // read what a body may.
        if !checks.iter().all(|&check| self.check_independent(check, &mut scan)) {
            return None;
        }
        let LoopScan { mut live, captures, .. } = scan;
        // The fork-join passes each of these as an `int` word, and a
        // `byte` is not one.
        let while_index = (!implicit_step).then_some(index);
        if [begin_local, end_local, while_index, Some(acc)].into_iter().flatten().any(|l| !self.is_int(l)) {
            return None;
        }
        // The bound locals are worker arguments already (lo/hi), not body
        // captures.
        for l in [begin_local, end_local, Some(index), Some(acc)].into_iter().flatten() {
            live.remove(self.name(l));
        }
        // The worker's arguments (`lo`, `hi`, the accumulator and the live
        // captures) fit one thread spawn.
        if 3 + live.len() > common::MAX_THREAD_SPAWN_ARGS {
            return None;
        }
        // The fork-join works on `[begin, end)`, so an inclusive end read
        // at run time is one it can add one to: not `MAX`, where the
        // sequential loop runs on and the sum would trap.
        if end_bias != 0 {
            let top = match end_local {
                Some(l) => ints.get(&l).copied().flatten().map(|r| r.hi),
                None => Some(end),
            };
            if top.is_none_or(|top| top == i64::MAX) {
                return None;
            }
        }
        // Every value the body reads the index at, for what `ints` holds
        // of the bounds. A `while` body may read it after its step too.
        let over = |ints: &IntLocals| {
            let first = match begin_local {
                Some(l) => ints.get(&l).copied().flatten().map(|r| r.lo),
                None => Some(begin),
            };
            let last = match end_local {
                Some(l) => ints.get(&l).copied().flatten().map(|r| r.hi as i128 + end_bias as i128 - 1),
                None => Some(end as i128 - 1),
            };
            match (first, last) {
                (Some(first), Some(last)) => Interval::cut(first as i128, last + if implicit_step { 0 } else { stride as i128 }),
                _ => None,
            }
        };
        // The fork-join reads these at the loop's start, and the loop
        // writes none of them.
        let mut invariants: Vec<LocalId> = Vec::new();
        for l in [begin_local, end_local].into_iter().flatten().chain(live.values().copied()) {
            if !invariants.contains(&l) {
                invariants.push(l);
            }
        }
        let guard = self.sign_guard(&invariants, ints, &|ints| self.one_signed(&forms, &checks, (index, over(ints)), consts, ints));
        let signed = guard.is_some();
        let name = |l: LocalId| self.name(l).to_string();
        let site = LoopParSite {
            index: name(index),
            begin,
            end,
            acc: name(acc),
            op,
            implicit_step,
            captures: captures.into_iter().collect(),
            live_captures: live.into_keys().collect(),
            stride,
            begin_local: begin_local.map(name),
            end_local: end_local.map(name),
            end_bias,
            guard: guard
                .unwrap_or_default()
                .into_iter()
                .map(|(l, at_least, bound)| SignTest { local: name(l), at_least, bound })
                .collect(),
        };
        Some((site, signed))
    }

    /// The tests of loop-invariant `vars` under which `signed` holds of
    /// `ints` narrowed by them: none when it holds as `ints` stand, or for
    /// both halves of a split of one of them (the values do not change
    /// while the loop runs, so in a run one half holds); else the first of
    /// one test of one of them, then of each of them alike, cut to the
    /// tests it needs. `None` when nothing tried signs it.
    ///
    /// A test is `v >= k` or `v <= k` for a small `k`: what the fork-join
    /// can make once, as the loop starts. A test no value passes is never
    /// taken.
    fn sign_guard(&self, vars: &[LocalId], ints: &IntLocals, signed: &dyn Fn(&IntLocals) -> bool) -> Option<Vec<(LocalId, bool, i64)>> {
        if signed(ints) {
            return Some(Vec::new());
        }
        let vars = &vars[..vars.len().min(GUARD_VARS)];
        let mut tries = GUARD_TRIES;
        let mut seen: HashMap<Vec<(LocalId, bool, i64)>, bool> = HashMap::new();
        let mut under = |tests: &[(LocalId, bool, i64)]| -> bool {
            if let Some(&known) = seen.get(tests) {
                return known;
            }
            if tries == 0 {
                return false;
            }
            tries -= 1;
            let mut env = ints.clone();
            let mut held = true;
            for &(l, at_least, k) in tests {
                let half = if at_least { Interval { lo: k, hi: i64::MAX } } else { Interval { lo: i64::MIN, hi: k } };
                match env.get(&l).copied().flatten().and_then(|r| r.meet(half)) {
                    Some(r) => {
                        env.insert(l, Some(r));
                    }
                    None => held = false,
                }
            }
            let ok = held && signed(&env);
            seen.insert(tests.to_vec(), ok);
            ok
        };
        for &v in vars {
            for t in GUARD_SPLITS {
                if under(&[(v, true, t)]) && under(&[(v, false, t - 1)]) {
                    return Some(Vec::new());
                }
            }
        }
        for &v in vars {
            for (at_least, k) in GUARD_TESTS {
                if under(&[(v, at_least, k)]) {
                    return Some(vec![(v, at_least, k)]);
                }
            }
        }
        if vars.len() > 1 {
            for (at_least, k) in GUARD_TESTS {
                let mut tests: Vec<_> = vars.iter().map(|&v| (v, at_least, k)).collect();
                if !under(&tests) {
                    continue;
                }
                // Drop each test the others do without.
                let mut i = 0;
                while i < tests.len() {
                    let mut fewer = tests.clone();
                    fewer.remove(i);
                    if !fewer.is_empty() && under(&fewer) {
                        tests = fewer;
                    } else {
                        i += 1;
                    }
                }
                return Some(tests);
            }
        }
        None
    }

    /// Whether the running fold of a loop with these `forms` leaves the
    /// `int` range only when the whole fold does, given every value its
    /// index takes (#2423).
    ///
    /// A chunk folds from the identity and the fork-join folds the
    /// partials, each step tested, so together they see the whole fold
    /// and each chunk's own. The sequential loop also traps on a running
    /// value that leaves the range and comes back. That cannot happen
    /// when:
    ///
    /// - `+`: every term is `>= 0`, or every term is `<= 0`. The running
    ///   sum only moves one way, so once past an end it stays past it.
    /// - `*`: every term is `>= 1` or `<= -2`. A value outside the range
    ///   is at least `2^63` in magnitude and not `-2^63`; multiplied by
    ///   such a term it stays so. A `0` or a `-1` could bring it back
    ///   (`2^63 * -1` is `MIN`).
    /// - `^`: it never traps.
    ///
    /// A term's sign is what [`Signs`] proves of it; unknown is refused.
    fn one_signed(&self, forms: &[StmtForm], checks: &[HirId], (index, over): (LocalId, Range), consts: &ConstLocals, ints: &IntLocals) -> bool {
        let mut env: Env = ints.clone();
        for (&l, v) in consts {
            if let ConstVal::Int(k) = v {
                env.insert(l, Some(Interval::point(*k)));
            }
        }
        env.insert(index, over);
        // A `for` runs its checks ahead of each iteration's body: one
        // that passed holds there.
        for &check in checks {
            if let HirKind::If { cond, then, els: None } = self.body.expr(check).kind
                && crate::hir::cold::fails(self.body, then, &|_| false)
                && !self.signs.assume(self.body, cond, false, &mut env)
            {
                return false;
            }
        }
        let mut fold = FoldSigns { up: true, down: true, product: true };
        self.fold_signs(forms, &mut env, &mut fold);
        fold.product && (fold.up || fold.down)
    }

    fn fold_signs(&self, forms: &[StmtForm], env: &mut Env, fold: &mut FoldSigns) {
        for form in forms {
            match form {
                StmtForm::Local { local, init } => {
                    let r = self.signs.range(self.body, *init, env);
                    env.insert(*local, r);
                }
                StmtForm::Reduce { op, expr, .. } => fold.note(*op, self.signs.range(self.body, *expr, env)),
                StmtForm::Step(_) => {}
                StmtForm::If { arms } => {
                    // Where every condition so far failed.
                    let mut rest = env.clone();
                    let mut open = true;
                    for arm in arms {
                        if !open {
                            break;
                        }
                        let mut inner = rest.clone();
                        let reached = match arm.cond {
                            Some(cond) => {
                                let reached = self.signs.assume(self.body, cond, true, &mut inner);
                                open = self.signs.assume(self.body, cond, false, &mut rest);
                                reached
                            }
                            None => true,
                        };
                        if reached {
                            self.fold_signs(&arm.body, &mut inner, fold);
                        }
                    }
                }
            }
        }
    }

    /// Narrow `ints` to where `cond` is `truth`, for the locals nothing
    /// in the body assigns: a fact that holds wherever it is read.
    fn assume(&self, cond: HirId, truth: bool, ints: &mut IntLocals) {
        let mut narrowed = ints.clone();
        if !self.signs.assume(self.body, cond, truth, &mut narrowed) {
            return;
        }
        for (l, r) in narrowed {
            if !self.written.contains(&l)
                && let Some(known) = ints.get_mut(&l)
            {
                *known = r;
            }
        }
    }

    fn forms_independent(&self, forms: &[StmtForm], locals: &mut HashSet<LocalId>, scan: &mut LoopScan<'_>) -> bool {
        for form in forms {
            match form {
                StmtForm::Local { local, init } => {
                    // The plan finds the index and the accumulator by name.
                    let name = self.name(*local);
                    if name == self.name(scan.index) || name == self.name(scan.acc) || !self.independent(*init, locals, scan) {
                        return false;
                    }
                    locals.insert(*local);
                }
                StmtForm::Reduce { expr, .. } => {
                    if !self.independent(*expr, locals, scan) {
                        return false;
                    }
                }
                StmtForm::Step(_) => {}
                StmtForm::If { arms } => {
                    for arm in arms {
                        if let Some(cond) = arm.cond
                            && !self.independent(cond, locals, scan)
                        {
                            return false;
                        }
                        if !self.forms_independent(&arm.body, &mut locals.clone(), scan) {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// Whether `id` reads nothing but the induction variable, loop-private
    /// temps, enclosing const ints, int locals/parameters, and integer
    /// literals, and calls nothing but pure functions.
    ///
    /// Division and modulo are admitted only with a non-zero literal
    /// divisor. Index, field, method, lambda, and `try` stay refused.
    /// Const ints are immediates in the worker; other int locals are extra
    /// arguments.
    fn independent(&self, id: HirId, locals: &HashSet<LocalId>, scan: &mut LoopScan<'_>) -> bool {
        // Walks recurse through here once per level of the tree.
        common::deep_stack(|| self.independent_level(id, locals, scan))
    }

    fn independent_level(&self, id: HirId, locals: &HashSet<LocalId>, scan: &mut LoopScan<'_>) -> bool {
        let body = self.body;
        match &body.expr(id).kind {
            HirKind::Lit(Lit::Int(_)) => true,
            HirKind::Local(l) => {
                if *l == scan.acc {
                    return false;
                }
                if *l == scan.index || locals.contains(l) {
                    return true;
                }
                // The plan finds the index by name, and `for _ in` names
                // its own `item`: a local of the program's by that name
                // would be taken for it.
                if self.name(*l) == self.name(scan.index) {
                    return false;
                }
                match scan.consts.get(l) {
                    Some(ConstVal::Int(k)) => {
                        scan.captures.insert((self.name(*l).to_string(), *k));
                        true
                    }
                    // A live capture is an `int` word of the worker's.
                    _ if scan.ints.contains_key(l) && self.is_int(*l) => {
                        scan.live.insert(self.name(*l).to_string(), *l);
                        true
                    }
                    _ => false,
                }
            }
            HirKind::Un { op: UnOp::Neg | UnOp::BitNot | UnOp::Not, operand } => self.independent(*operand, locals, scan),
            HirKind::Logic { lhs, rhs, .. } => self.independent(*lhs, locals, scan) && self.independent(*rhs, locals, scan),
            HirKind::Bin { op, lhs, rhs } => {
                let admitted = match source_op(*op) {
                    "+" | "-" | "*" | "<<" | ">>" | "^" | "&" | "|" | "==" | "!=" | "<" | ">" | "<=" | ">=" => true,
                    "/" | "%" => int_literal(body, *rhs).is_some_and(|k| k != 0),
                    _ => false,
                };
                admitted && self.independent(*lhs, locals, scan) && self.independent(*rhs, locals, scan)
            }
            HirKind::Call { args, .. } => {
                super::par_profit::bare_callee(self.checker, body, id).is_some_and(|name| self.pure_fns.contains(name))
                    && args.iter().all(|a| self.independent(*a, locals, scan))
            }
            _ => false,
        }
    }

    /// Whether contract check `id`, as the builder writes one (`if
    /// !(cond) { panic .. }`, or nothing for a clause `coil verify`
    /// proved), tests a condition a loop body could compute
    /// ([`Self::independent`]) and reports it reading no local.
    fn check_independent(&self, id: HirId, scan: &mut LoopScan<'_>) -> bool {
        let body = self.body;
        match &body.expr(id).kind {
            HirKind::Block { stmts, tail: None } => stmts.is_empty(),
            &HirKind::If { cond, then, els: None } => {
                let HirKind::Un { op: UnOp::Not, operand } = body.expr(cond).kind else { return false };
                let mut reads = false;
                body.visit(then, &mut |e| reads |= matches!(e.kind, HirKind::Local(_)));
                !reads && self.independent(operand, &HashSet::new(), scan)
            }
            _ => false,
        }
    }

    /// Classify a loop body. `allow_step` is false inside `if` (the latch
    /// stays outside).
    fn classify_body(&self, id: HirId, index: LocalId, allow_step: bool) -> Option<Vec<StmtForm>> {
        match &self.body.expr(id).kind {
            HirKind::Block { stmts, tail } => stmts.iter().chain(tail).map(|item| self.statement_form(*item, index, allow_step)).collect(),
            _ => Some(vec![self.statement_form(id, index, allow_step)?]),
        }
    }

    /// Classify one body statement; `None` rejects the whole loop.
    fn statement_form(&self, id: HirId, index: LocalId, allow_step: bool) -> Option<StmtForm> {
        let body = self.body;
        let e = body.expr(id);
        match e.kind {
            HirKind::Let { local, init: Some(init) } if body.local(local).kind == LocalKind::Let => Some(StmtForm::Local { local, init }),
            HirKind::If { .. } => {
                let mut arms = Vec::new();
                let mut at = Some(id);
                while let Some(branch) = at {
                    match body.expr(branch).kind {
                        HirKind::If { cond, then, els } => {
                            arms.push(IfArm { cond: Some(cond), body: self.classify_body(then, index, false)? });
                            at = els;
                        }
                        _ => {
                            arms.push(IfArm { cond: None, body: self.classify_body(branch, index, false)? });
                            at = None;
                        }
                    }
                }
                Some(StmtForm::If { arms })
            }
            HirKind::Assign { place, value } => {
                let target = local_read(body, place)?;
                let HirKind::Bin { op, lhs, rhs } = body.expr(unwrap_byte(body, value)).kind else {
                    return None;
                };
                let op = source_op(op);
                if e.flags.contains(HirFlags::ADJUST) {
                    return (allow_step && op == "+" && target == index).then_some(StmtForm::Step(1));
                }
                if e.flags.contains(HirFlags::COMPOUND) {
                    if target == index {
                        let k = (op == "+" && allow_step).then(|| int_literal(body, rhs)).flatten().filter(|k| *k > 0)?;
                        return Some(StmtForm::Step(k));
                    }
                    return Some(StmtForm::Reduce { acc: target, op: reduce_op(op)?, expr: rhs });
                }
                let reads = |x: HirId, l: LocalId| local_read(body, x) == Some(l);
                if target == index {
                    // `i = i + k` or `i = k + i` with `k > 0`.
                    if !allow_step || op != "+" {
                        return None;
                    }
                    let k = if reads(lhs, index) {
                        int_literal(body, rhs)
                    } else if reads(rhs, index) {
                        int_literal(body, lhs)
                    } else {
                        None
                    }?;
                    return (k > 0).then_some(StmtForm::Step(k));
                }
                // `acc = acc ⊕ expr` or `acc = expr ⊕ acc` for commutative ops.
                let op = reduce_op(op)?;
                let expr = if reads(lhs, target) {
                    rhs
                } else if reads(rhs, target) {
                    lhs
                } else {
                    return None;
                };
                Some(StmtForm::Reduce { acc: target, op, expr })
            }
            _ => None,
        }
    }

    /// What statement `item` leaves known of the locals it binds or writes.
    fn note_binding_effects(&self, item: HirId, consts: &mut ConstLocals, ints: &mut IntLocals) {
        let body = self.body;
        let e = body.expr(item);
        match e.kind {
            HirKind::Assign { place, value } if !e.flags.contains(HirFlags::ADJUST) => {
                let Some(l) = local_read(body, place) else { return };
                consts.remove(&l);
                // `x op= e` holds an int when `e` is one.
                let rhs = if e.flags.contains(HirFlags::COMPOUND) {
                    match body.expr(unwrap_byte(body, value)).kind {
                        HirKind::Bin { rhs, .. } => Some(rhs),
                        _ => None,
                    }
                } else {
                    Some(value)
                };
                if rhs.is_some_and(|rhs| is_int_expr(body, rhs, ints)) {
                    ints.insert(l, Some(Interval::of_type(body.local(l).ty.as_ref())));
                } else {
                    ints.remove(&l);
                }
            }
            _ => {
                for l in self.assigned(item) {
                    consts.remove(&l);
                    ints.remove(&l);
                }
            }
        }
        // A local `const` is a `let` nothing writes again.
        if let HirKind::Let { local, init: Some(init) } = e.kind
            && matches!(body.local(local).kind, LocalKind::Let | LocalKind::Const)
        {
            if let Some(v) = const_val(body, init, consts) {
                consts.insert(local, v);
            }
            if is_int_expr(body, init, ints) {
                let floor = Interval::of_type(body.local(local).ty.as_ref());
                let held = self.signs.range(body, init, ints).and_then(|r| r.meet(floor));
                ints.insert(local, Some(held.unwrap_or(floor)));
            }
        }
        // An `if` one of whose branches always returns was not taken
        // that way.
        if let HirKind::If { cond, then, els } = e.kind {
            if query::exits(body, then) || crate::hir::cold::fails(body, then, &|_| false) {
                self.assume(cond, false, ints);
            } else if els.is_some_and(|els| query::exits(body, els)) {
                self.assume(cond, true, ints);
            }
        }
    }

    /// The locals written anywhere inside `id`, a lambda's writes of its
    /// captures included.
    fn assigned(&self, id: HirId) -> HashSet<LocalId> {
        let mut out = HashSet::new();
        assigned_in(self.module, self.body, id, &mut out);
        out
    }
}

pub(super) fn assigned_in(module: &HirModule, body: &HirBody, id: HirId, out: &mut HashSet<LocalId>) {
    body.visit(id, &mut |e| match &e.kind {
        HirKind::Assign { place, .. } => {
            if let Some(l) = local_read(body, *place) {
                out.insert(l);
            }
        }
        HirKind::Lambda { body: index } => {
            let Some(lambda) = module.bodies.get(*index) else { return };
            let Some(root) = lambda.root else { return };
            let mut inner = HashSet::new();
            assigned_in(module, lambda, root, &mut inner);
            out.extend(lambda.captures.iter().filter(|(_, own)| inner.contains(own)).map(|(outer, _)| *outer));
        }
        _ => {}
    });
}

/// The loop-invariant locals [`Scan::sign_guard`] tests, at most: the
/// bounds first, then the live captures by name.
const GUARD_VARS: usize = 4;

/// Narrowed ranges [`Scan::sign_guard`] evaluates a loop's fold for, at
/// most.
const GUARD_TRIES: usize = 48;

/// The splits [`Scan::sign_guard`] tries: `v >= t` against `v <= t - 1`.
const GUARD_SPLITS: [i64; 3] = [0, 1, -1];

/// The tests [`Scan::sign_guard`] tries, weakest first: `(at_least, k)`.
/// The sign rule's edges are among them (`>= 0` / `<= 0` for a sum,
/// `>= 1` / `<= -2` for a product), and `2` for a term one below its
/// capture.
const GUARD_TESTS: [(bool, i64); 6] = [(true, 0), (true, 1), (true, 2), (false, 0), (false, -1), (false, -2)];

/// What the terms of a loop's fold allow ([`Scan::one_signed`]).
struct FoldSigns {
    /// Every `+` term so far is `>= 0`.
    up: bool,
    /// Every `+` term so far is `<= 0`.
    down: bool,
    /// Every `*` term so far is `>= 1` or `<= -2`.
    product: bool,
}

impl FoldSigns {
    fn note(&mut self, op: LoopReduceOp, term: Range) {
        // A term with no value at all is not one to reason from.
        let Some(term) = term else {
            self.product = false;
            return;
        };
        match op {
            LoopReduceOp::Add => {
                self.up &= term.lo >= 0;
                self.down &= term.hi <= 0;
            }
            LoopReduceOp::Mul => self.product &= term.lo >= 1 || term.hi <= -2,
            LoopReduceOp::Xor => {}
        }
    }
}

fn collect_forms(forms: &[StmtForm], reduces: &mut Vec<(LocalId, LoopReduceOp)>, steps: &mut Vec<i64>) {
    for form in forms {
        match form {
            StmtForm::Step(k) => steps.push(*k),
            StmtForm::Reduce { acc, op, .. } => reduces.push((*acc, *op)),
            StmtForm::Local { .. } => {}
            StmtForm::If { arms } => {
                for arm in arms {
                    collect_forms(&arm.body, reduces, steps);
                }
            }
        }
    }
}

fn iteration_count(begin: i64, end: i64, stride: i64) -> Option<i64> {
    if stride <= 0 {
        return None;
    }
    let diff = end.checked_sub(begin)?;
    if diff <= 0 {
        return Some(0);
    }
    diff.checked_add(stride - 1)?.checked_div(stride)
}

/// The operator as written, whatever lane the types gave it.
pub(super) fn source_op(op: BinOp) -> &'static str {
    match op {
        BinOp::IntAdd | BinOp::FloatAdd | BinOp::StrConcat => "+",
        BinOp::IntSub | BinOp::FloatSub => "-",
        BinOp::IntMul | BinOp::FloatMul => "*",
        BinOp::IntDiv | BinOp::FloatDiv => "/",
        BinOp::IntRem | BinOp::FloatRem => "%",
        BinOp::IntPow | BinOp::FloatPow => "**",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::Overloaded(op) => op,
    }
}

fn reduce_op(op: &str) -> Option<LoopReduceOp> {
    match op {
        "+" => Some(LoopReduceOp::Add),
        "*" => Some(LoopReduceOp::Mul),
        "^" => Some(LoopReduceOp::Xor),
        _ => None,
    }
}

/// `id` under the `& 255` the builder wraps `byte` arithmetic in.
pub(super) fn unwrap_byte(body: &HirBody, id: HirId) -> HirId {
    let e = body.expr(id);
    if e.node.is_none()
        && let HirKind::Bin { op: BinOp::BitAnd, lhs, rhs } = e.kind
        && body.expr(rhs).node.is_none()
        && matches!(body.expr(rhs).kind, HirKind::Lit(Lit::Int(255)))
        && body.expr(lhs).span == e.span
    {
        return lhs;
    }
    id
}

fn local_read(body: &HirBody, id: HirId) -> Option<LocalId> {
    match body.expr(id).kind {
        HirKind::Local(l) => Some(l),
        _ => None,
    }
}

fn int_literal(body: &HirBody, id: HirId) -> Option<i64> {
    match body.expr(id).kind {
        HirKind::Lit(Lit::Int(k)) => Some(k),
        _ => None,
    }
}

/// Const int, or an int local. `(local, const value)`.
fn int_endpoint(body: &HirBody, id: HirId, consts: &ConstLocals, ints: &IntLocals) -> Option<(Option<LocalId>, i64)> {
    if let Some(k) = const_int(body, id, consts) {
        return Some((None, k));
    }
    let l = local_read(body, id)?;
    ints.contains_key(&l).then_some((Some(l), 0))
}

fn const_val(body: &HirBody, id: HirId, consts: &ConstLocals) -> Option<ConstVal> {
    if let Some(k) = const_int(body, id, consts) {
        return Some(ConstVal::Int(k));
    }
    match &body.expr(id).kind {
        HirKind::Make { kind: MakeKind::Range { inclusive }, args } => {
            let [start, end] = args.as_slice() else { return None };
            let begin = const_int(body, *start, consts)?;
            let end = const_int(body, *end, consts)?;
            let end = if *inclusive { end.checked_add(1)? } else { end };
            Some(ConstVal::Range { begin, end })
        }
        HirKind::Local(l) => consts.get(l).copied(),
        _ => None,
    }
}

fn const_int(body: &HirBody, id: HirId, consts: &ConstLocals) -> Option<i64> {
    match body.expr(id).kind {
        HirKind::Lit(Lit::Int(k)) => Some(k),
        HirKind::Local(l) => match consts.get(&l) {
            Some(ConstVal::Int(k)) => Some(*k),
            _ => None,
        },
        _ => None,
    }
}

/// Whether `id` is an int by its shape: literals, int locals and the
/// arithmetic over them.
fn is_int_expr(body: &HirBody, id: HirId, ints: &IntLocals) -> bool {
    match body.expr(id).kind {
        HirKind::Lit(Lit::Int(_)) => true,
        HirKind::Local(l) => ints.contains_key(&l),
        HirKind::Un { op: UnOp::Neg, operand } => is_int_expr(body, operand, ints),
        HirKind::Bin { op, lhs, rhs } => {
            matches!(source_op(op), "+" | "-" | "*" | "<<" | ">>" | "^" | "&" | "|") && is_int_expr(body, lhs, ints) && is_int_expr(body, rhs, ints)
        }
        _ => false,
    }
}

#[cfg(test)]
#[path = "loop_par.tests.rs"]
mod tests;
