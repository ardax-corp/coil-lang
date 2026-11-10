//! Independent Parallel Arms for counted loops (loop IPA).
//!
//! The recursive IPA in [`par_profit`](crate::typechecking::par_profit)
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

use std::collections::{BTreeSet, HashMap, HashSet};

use super::super::{BinOp, BodyKind, Callee, HirBody, HirFlags, HirId, HirKind, HirModule, HirPat, Lit, LocalId, LocalKind, MakeKind, UnOp, query};
use crate::typechecking::par_profit::DEFAULT_LOOP_GRAIN;
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
    /// Every counted-loop fork site of the module.
    ///
    /// `pure_fns` is the side-effect-free user functions; a loop body may
    /// only call those.
    pub fn loop_par_sites(&self, pure_fns: &HashSet<String>) -> LoopParSites {
        let mut out = LoopParSites::new();
        for body in &self.bodies {
            let Some(root) = body.root else { continue };
            // The top level holds declarations only; a lambda, a test and
            // a static's initializer run sequentially.
            if !matches!(body.kind, BodyKind::Function | BodyKind::Method) || self.in_trait(body) {
                continue;
            }
            let mut ints: HashSet<LocalId> = body
                .params
                .iter()
                .copied()
                .filter(|&p| matches!(&body.local(p).ty, Some(Ty::Con(n)) if n == coil_ty::INT || n == coil_ty::BYTE))
                .collect();
            let mut scan = Scan { module: self, body, pure_fns, out: &mut out };
            scan.walk(root, &mut ConstLocals::new(), &mut ints);
        }
        out
    }

    /// Whether `body` is a trait's method, an instance's or a default, or
    /// a function declared in one. The AST walk this analysis replaced
    /// looked at functions and inherent methods only, and it keeps to those.
    fn in_trait(&self, body: &HirBody) -> bool {
        let path: Vec<&str> = body.name.split("::").collect();
        path.iter().any(|segment| segment.contains(" for "))
            || path.windows(2).any(|pair| self.trait_effects.iter().any(|t| t.trait_name == pair[0] && t.method == pair[1]))
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

struct Scan<'a> {
    module: &'a HirModule,
    body: &'a HirBody,
    pure_fns: &'a HashSet<String>,
    out: &'a mut LoopParSites,
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
}

struct LoopScan<'a> {
    index: LocalId,
    acc: LocalId,
    consts: &'a ConstLocals,
    ints: &'a HashSet<LocalId>,
    captures: BTreeSet<(String, i64)>,
    live: BTreeSet<String>,
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

    /// Statement list: each item may bind or invalidate a const local for
    /// the statements that follow it. `ints` is every local still known to
    /// be an int.
    fn walk_block(&mut self, items: &[HirId], consts: &mut ConstLocals, ints: &mut HashSet<LocalId>) {
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
    fn walk_loop_body(&mut self, loop_id: HirId, body: HirId, consts: &ConstLocals, ints: &HashSet<LocalId>) {
        let mut inner = consts.clone();
        let mut inner_ints = ints.clone();
        for l in self.assigned(loop_id) {
            inner.remove(&l);
            inner_ints.remove(&l);
        }
        self.walk(body, &mut inner, &mut inner_ints);
    }

    fn walk(&mut self, id: HirId, consts: &mut ConstLocals, ints: &mut HashSet<LocalId>) {
        let body = self.body;
        let e = body.expr(id);
        if e.flags.contains(HirFlags::IF_LET) {
            return;
        }
        match &e.kind {
            // A nested block's own bindings do not outlive it.
            HirKind::Block { stmts, tail } => {
                let items: Vec<HirId> = stmts.iter().copied().chain(*tail).collect();
                self.walk_block(&items, &mut consts.clone(), &mut ints.clone());
            }
            HirKind::Let { init: Some(init), .. } => self.walk(*init, &mut consts.clone(), &mut ints.clone()),
            HirKind::Return(Some(value)) => self.walk(*value, consts, ints),
            HirKind::If { then, els, .. } => {
                self.walk(*then, &mut consts.clone(), &mut ints.clone());
                if let Some(els) = els {
                    self.walk(*els, &mut consts.clone(), &mut ints.clone());
                }
            }
            HirKind::Match { arms, .. } => {
                for arm in arms {
                    self.walk(arm.body, &mut consts.clone(), &mut ints.clone());
                }
            }
            HirKind::Loop { body: loop_body } => {
                // A `while let` is no counted loop, and nothing in it is.
                let Some((_, cond, then)) = query::while_parts(body, *loop_body) else { return };
                match self.counted_while(cond, then, consts, ints).and_then(|c| self.finish(c, consts, ints)) {
                    Some(site) => {
                        self.out.insert(e.span, site);
                    }
                    None => self.walk_loop_body(id, then, consts, ints),
                }
            }
            HirKind::ForIn { pat, iterable, .. } => {
                let Some((_, own)) = query::for_body_parts(body, id) else { return };
                match self.counted_for(pat, *iterable, own, consts, ints).and_then(|c| self.finish(c, consts, ints)) {
                    Some(site) => {
                        self.out.insert(e.span, site);
                    }
                    None => self.walk_loop_body(id, own, consts, ints),
                }
            }
            _ => {}
        }
    }

    /// `while i < K { … }` against the loop-IPA shape.
    fn counted_while(&self, cond: HirId, then: HirId, consts: &ConstLocals, ints: &HashSet<LocalId>) -> Option<Counted> {
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
        } else if ints.contains(&index) {
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
        Some(Counted { body: then, index, begin, end, begin_local, end_local, end_bias, implicit_step: false })
    }

    /// `for x in START..END` / `..=`, or `for x in r` when `r` is a
    /// const-initialized counted range local (Q6 literal / B5).
    fn counted_for(&self, pat: &HirPat, iterable: HirId, own: HirId, consts: &ConstLocals, ints: &HashSet<LocalId>) -> Option<Counted> {
        let body = self.body;
        let HirPat::Bind(index) = *pat else { return None };
        // `for mut x in` borrows; `for _ in` binds nothing a body names.
        if body.local(index).borrow || body.local(index).kind != LocalKind::Pattern {
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
        Some(Counted { body: own, index, begin, end, begin_local, end_local, end_bias, implicit_step: true })
    }

    fn finish(&self, counted: Counted, consts: &ConstLocals, ints: &HashSet<LocalId>) -> Option<LoopParSite> {
        let Counted { body: loop_body, index, begin, end, begin_local, end_local, end_bias, implicit_step } = counted;
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
        let mut scan = LoopScan { index, acc, consts, ints, captures: BTreeSet::new(), live: BTreeSet::new() };
        if !self.forms_independent(&forms, &mut HashSet::new(), &mut scan) {
            return None;
        }
        let LoopScan { mut live, captures, .. } = scan;
        // The bound locals are worker arguments already (lo/hi), not body
        // captures.
        for l in [begin_local, end_local, Some(index), Some(acc)].into_iter().flatten() {
            live.remove(self.name(l));
        }
        let name = |l: LocalId| self.name(l).to_string();
        Some(LoopParSite {
            index: name(index),
            begin,
            end,
            acc: name(acc),
            op,
            implicit_step,
            captures: captures.into_iter().collect(),
            live_captures: live.into_iter().collect(),
            stride,
            begin_local: begin_local.map(name),
            end_local: end_local.map(name),
            end_bias,
        })
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
                match scan.consts.get(l) {
                    Some(ConstVal::Int(k)) => {
                        scan.captures.insert((self.name(*l).to_string(), *k));
                        true
                    }
                    _ if scan.ints.contains(l) => {
                        scan.live.insert(self.name(*l).to_string());
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
            // A bare name: a path or a method is not in `pure_fns` as written.
            HirKind::Call { callee: Callee::Named { name, .. }, args } => {
                !name.contains("::") && self.pure_fns.contains(name) && args.iter().all(|a| self.independent(*a, locals, scan))
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
    fn note_binding_effects(&self, item: HirId, consts: &mut ConstLocals, ints: &mut HashSet<LocalId>) {
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
                    ints.insert(l);
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
        if let HirKind::Let { local, init: Some(init) } = e.kind
            && body.local(local).kind == LocalKind::Let
        {
            if let Some(v) = const_val(body, init, consts) {
                consts.insert(local, v);
            }
            if is_int_expr(body, init, ints) {
                ints.insert(local);
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

fn assigned_in(module: &HirModule, body: &HirBody, id: HirId, out: &mut HashSet<LocalId>) {
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
fn source_op(op: BinOp) -> &'static str {
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
fn unwrap_byte(body: &HirBody, id: HirId) -> HirId {
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
fn int_endpoint(body: &HirBody, id: HirId, consts: &ConstLocals, ints: &HashSet<LocalId>) -> Option<(Option<LocalId>, i64)> {
    if let Some(k) = const_int(body, id, consts) {
        return Some((None, k));
    }
    let l = local_read(body, id)?;
    ints.contains(&l).then_some((Some(l), 0))
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
fn is_int_expr(body: &HirBody, id: HirId, ints: &HashSet<LocalId>) -> bool {
    match body.expr(id).kind {
        HirKind::Lit(Lit::Int(_)) => true,
        HirKind::Local(l) => ints.contains(&l),
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
