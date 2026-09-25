//! Independent Parallel Arms for counted loops (loop IPA).
//!
//! The recursive IPA in [`par_profit`](super::par_profit) treats sibling
//! self-calls as independent arms. A counted loop is the same idea with the
//! arms spread over an induction range: when the iterations only communicate
//! through one associative reduction `acc = acc ⊕ e(i)`, any partition of the
//! range folds to the sequential result.
//!
//! Detection is structural — no function, module or benchmark allowlists — and
//! fails closed. Anything the walk cannot prove independent (a nested loop, an
//! impure call, a second reduction, a non-int capture) leaves the loop
//! sequential. Counted `for x in` / range (Q6 literal, B5 const range locals)
//! share this shape; the latch is implicit.
//!
//! Wide shapes (`COIL_PAR_LOOP_WIDE`, default on) also admit a dynamic int
//! bound, an int parameter or int local capture, a pure `if` whose arms share
//! one reduction, and a positive constant stride. `COIL_PAR_LOOP_WIDE=0`
//! keeps the original const unit-step shape.

use std::collections::{BTreeSet, HashMap, HashSet};

use parser::ast::{AdjustOp, AssignOp, Expression, Output};

use super::par_profit::{par_loop_grain, par_loop_wide_enabled};

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
    /// Pointer of the induction identifier's `Expression` (sidecar lookup).
    pub index_expr_ptr: usize,
    /// Pointer of `e` in `acc = acc ⊕ e` (the first reduction; `if` arms may hold more).
    pub reduce_expr_ptr: usize,
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

/// Detected sites keyed by the loop node's source span (codegen's join key).
pub type LoopParSites = HashMap<(usize, usize), LoopParSite>;

/// Collect every counted-loop fork site in `ast`.
///
/// `pure_fns` is the side-effect-free user functions
/// ([`analyze_pure_fns`](super::purity::analyze_pure_fns)); a loop body may
/// only call those.
pub fn analyze_loop_par_sites(ast: &Output<'_>, pure_fns: &HashSet<String>) -> LoopParSites {
    let mut scan = Scan {
        pure_fns,
        out: LoopParSites::new(),
    };
    scan.walk(ast, &mut ConstLocals::new(), &mut HashSet::new());
    scan.out
}

/// Locals proven to hold a compile-time int or counted range at this point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConstVal {
    Int(i64),
    /// Half-open `[begin, end)`. Inclusive source ranges are normalized here.
    Range {
        begin: i64,
        end: i64,
    },
}

/// Locals proven to hold a compile-time int or counted range at this point.
type ConstLocals = HashMap<String, ConstVal>;

struct Scan<'a> {
    pure_fns: &'a HashSet<String>,
    out: LoopParSites,
}

impl Scan<'_> {
    /// Statement list: each item may bind or invalidate a const local for the
    /// statements that follow it. `ints` is every name still known to be an int.
    fn walk_block(
        &mut self,
        items: &[Output<'_>],
        consts: &mut ConstLocals,
        ints: &mut HashSet<String>,
    ) {
        for item in items {
            self.walk(item, consts, ints);
            note_binding_effects(item, consts, ints);
        }
    }

    /// Walk a loop body that is *not* a fork site.
    ///
    /// Everything the loop assigns is dropped first: a const local's binding no
    /// longer describes every visit to a program point inside a loop.
    fn walk_loop_body(
        &mut self,
        loop_node: &Output<'_>,
        body: &Output<'_>,
        consts: &ConstLocals,
        ints: &HashSet<String>,
    ) {
        let mut inner = consts.clone();
        let mut inner_ints = ints.clone();
        for name in assigned_names(loop_node) {
            inner.remove(&name);
            inner_ints.remove(&name);
        }
        self.walk(body, &mut inner, &mut inner_ints);
    }

    fn walk(&mut self, ast: &Output<'_>, consts: &mut ConstLocals, ints: &mut HashSet<String>) {
        match ast.1.as_ref() {
            // A nested block's own bindings do not outlive it.
            Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
                self.walk_block(items, &mut consts.clone(), &mut ints.clone());
            }
            Expression::Module(_, inner)
            | Expression::Statement(inner)
            | Expression::Expr(inner)
            | Expression::ExprStatement(inner)
            | Expression::Group(inner)
            | Expression::Return(inner)
            | Expression::ImplicitReturn(inner) => self.walk(inner, consts, ints),
            Expression::Function {
                args,
                body: Some(body),
                ..
            } => {
                let mut ints = int_param_names(args);
                self.walk(body, &mut ConstLocals::new(), &mut ints);
            }
            Expression::Implementation { methods, .. } => {
                for m in methods {
                    self.walk(m, &mut ConstLocals::new(), &mut HashSet::new());
                }
            }
            Expression::Method(_, inner) | Expression::Member(inner) => {
                self.walk(inner, &mut ConstLocals::new(), &mut HashSet::new());
            }
            Expression::If(branches) => {
                for b in branches {
                    self.walk(b, &mut consts.clone(), &mut ints.clone());
                }
            }
            Expression::Branch(_, body) => self.walk(body, &mut consts.clone(), &mut ints.clone()),
            Expression::Match { arms, .. } => {
                for arm in arms {
                    self.walk(&arm.body, &mut consts.clone(), &mut ints.clone());
                }
            }
            Expression::Loop {
                identifier: None,
                pattern: None,
                iterable,
                body,
            } => match self.match_counted_while(iterable, body, consts, ints) {
                Some(site) => {
                    self.out.insert((ast.0.start, ast.0.end), site);
                }
                None => self.walk_loop_body(ast, body, consts, ints),
            },
            Expression::Loop {
                identifier: Some(binding),
                pattern: None,
                iterable,
                body,
            } => match self.match_counted_for_range(binding, iterable, body, consts, ints) {
                Some(site) => {
                    self.out.insert((ast.0.start, ast.0.end), site);
                }
                None => self.walk_loop_body(ast, body, consts, ints),
            },
            Expression::Loop { body, .. } => self.walk_loop_body(ast, body, consts, ints),
            _ => {}
        }
    }

    /// Match `while i < K { … }` against the loop-IPA shape.
    fn match_counted_while(
        &self,
        cond: &Output<'_>,
        body: &Output<'_>,
        consts: &ConstLocals,
        ints: &HashSet<String>,
    ) -> Option<LoopParSite> {
        let (index, index_expr_ptr, end_local, end_const, inclusive) =
            counted_bound(cond, consts, ints)?;
        let (begin, begin_local) = if let Some(ConstVal::Int(b)) = consts.get(&index) {
            (*b, None)
        } else if ints.contains(&index) {
            (0, Some(index.clone()))
        } else {
            return None;
        };
        let (end, end_bias) = if end_local.is_some() {
            (end_const, if inclusive { 1 } else { 0 })
        } else if inclusive {
            (end_const.checked_add(1)?, 0)
        } else {
            (end_const, 0)
        };
        self.finish_counted_site(
            body,
            index,
            index_expr_ptr,
            begin,
            end,
            begin_local,
            end_local,
            end_bias,
            consts,
            ints,
            false,
        )
    }

    /// Match `for x in START..END` / `..=`, or `for x in r` when `r` is a
    /// const-initialized counted range local (Q6 literal / B5). Dynamic C2
    /// params stay sequential — no runtime trip-count tax.
    fn match_counted_for_range(
        &self,
        binding: &Output<'_>,
        iterable: &Output<'_>,
        body: &Output<'_>,
        consts: &ConstLocals,
        ints: &HashSet<String>,
    ) -> Option<LoopParSite> {
        let binding = peel(binding);
        let index = ident_name(binding)?;
        let (begin, end, begin_local, end_local, end_bias) = counted_range(iterable, consts, ints)?;
        self.finish_counted_site(
            body,
            index.to_string(),
            std::ptr::from_ref(binding) as *const Output<'_> as usize,
            begin,
            end,
            begin_local,
            end_local,
            end_bias,
            consts,
            ints,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_counted_site(
        &self,
        body: &Output<'_>,
        index: String,
        index_expr_ptr: usize,
        begin: i64,
        end: i64,
        begin_local: Option<String>,
        end_local: Option<String>,
        end_bias: i64,
        consts: &ConstLocals,
        ints: &HashSet<String>,
        implicit_step: bool,
    ) -> Option<LoopParSite> {
        let dynamic = begin_local.is_some() || end_local.is_some();
        let forms = classify_body(body, &index, true)?;
        let mut reduces: Vec<(&str, LoopReduceOp, &Output<'_>)> = Vec::new();
        let mut steps = Vec::new();
        let mut has_if = false;
        collect_forms(&forms, &mut reduces, &mut steps, &mut has_if);
        let expect_steps = if implicit_step { 0 } else { 1 };
        if steps.len() != expect_steps {
            return None;
        }
        let stride = if implicit_step { 1 } else { steps[0] };
        if stride <= 0 {
            return None;
        }
        if !dynamic {
            let trips = iteration_count(begin, end, stride)?;
            if trips <= par_loop_grain() {
                return None;
            }
        }
        let (acc, op, reduce_expr) = *reduces.first()?;
        if reduces.iter().any(|(a, o, _)| *a != acc || *o != op) {
            return None;
        }
        if acc == index {
            return None;
        }
        // The accumulator must be a const-initialized local of an enclosing
        // scope: that proves it is a frame slot codegen can find, and that no
        // earlier statement left it with an unknown value.
        if !matches!(consts.get(acc), Some(ConstVal::Int(_))) {
            return None;
        }

        let mut locals = HashSet::new();
        let mut captures = BTreeSet::new();
        let mut live = BTreeSet::new();
        if !self.forms_independent(
            &forms,
            &index,
            acc,
            &mut locals,
            consts,
            ints,
            &mut captures,
            &mut live,
        ) {
            return None;
        }
        // The bound locals are worker arguments already (lo/hi), not body captures.
        if let Some(name) = &begin_local {
            live.remove(name);
        }
        if let Some(name) = &end_local {
            live.remove(name);
        }
        live.remove(&index);
        live.remove(acc);

        let wide = dynamic || stride != 1 || !live.is_empty() || has_if;
        if wide && !par_loop_wide_enabled() {
            return None;
        }

        Some(LoopParSite {
            index,
            begin,
            end,
            acc: acc.to_string(),
            op,
            implicit_step,
            index_expr_ptr,
            reduce_expr_ptr: std::ptr::from_ref(reduce_expr) as *const Output<'_> as usize,
            captures: captures.into_iter().collect(),
            live_captures: live.into_iter().collect(),
            stride,
            begin_local,
            end_local,
            end_bias,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn forms_independent(
        &self,
        forms: &[StmtForm<'_>],
        index: &str,
        acc: &str,
        locals: &mut HashSet<String>,
        consts: &ConstLocals,
        ints: &HashSet<String>,
        captures: &mut BTreeSet<(String, i64)>,
        live: &mut BTreeSet<String>,
    ) -> bool {
        for form in forms {
            match form {
                StmtForm::Local { name, init } => {
                    if *name == index
                        || *name == acc
                        || !self.independent(init, index, acc, locals, consts, ints, captures, live)
                    {
                        return false;
                    }
                    locals.insert((*name).to_string());
                }
                StmtForm::Reduce { expr, .. } => {
                    if !self.independent(expr, index, acc, locals, consts, ints, captures, live) {
                        return false;
                    }
                }
                StmtForm::Step(_) => {}
                StmtForm::If { arms } => {
                    for arm in arms {
                        if let Some(cond) = arm.cond
                            && !self
                                .independent(cond, index, acc, locals, consts, ints, captures, live)
                        {
                            return false;
                        }
                        let mut inner = locals.clone();
                        if !self.forms_independent(
                            &arm.body, index, acc, &mut inner, consts, ints, captures, live,
                        ) {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// Whether `expr` reads nothing but the induction variable, loop-private
    /// temps, enclosing const ints, int locals/parameters, and integer literals,
    /// and calls nothing but pure functions.
    ///
    /// Division and modulo are admitted only with a non-zero literal divisor.
    /// Index, field, method, lambda, and `try` stay refused. Const ints are
    /// immediates in the worker; other int names are extra arguments.
    #[allow(clippy::too_many_arguments)]
    fn independent(
        &self,
        expr: &Output<'_>,
        index: &str,
        acc: &str,
        locals: &HashSet<String>,
        consts: &ConstLocals,
        ints: &HashSet<String>,
        captures: &mut BTreeSet<(String, i64)>,
        live: &mut BTreeSet<String>,
    ) -> bool {
        let expr = peel(expr);
        let both = |a: &Output<'_>,
                    b: &Output<'_>,
                    captures: &mut BTreeSet<(String, i64)>,
                    live: &mut BTreeSet<String>| {
            self.independent(a, index, acc, locals, consts, ints, captures, live)
                && self.independent(b, index, acc, locals, consts, ints, captures, live)
        };
        match expr.1.as_ref() {
            Expression::Integer(_) => true,
            Expression::Identifier(n) => {
                if *n == acc {
                    return false;
                }
                if *n == index || locals.contains(*n) {
                    return true;
                }
                match consts.get(*n) {
                    Some(ConstVal::Int(k)) => {
                        captures.insert(((*n).to_string(), *k));
                        true
                    }
                    _ if ints.contains(*n) => {
                        live.insert((*n).to_string());
                        true
                    }
                    _ => false,
                }
            }
            Expression::Negate(a)
            | Expression::Positive(a)
            | Expression::Not(a)
            | Expression::LogicalNot(a) => {
                self.independent(a, index, acc, locals, consts, ints, captures, live)
            }
            Expression::Add(a, b)
            | Expression::Sub(a, b)
            | Expression::Mul(a, b)
            | Expression::Shl(a, b)
            | Expression::Shr(a, b)
            | Expression::Xor(a, b)
            | Expression::BitAnd(a, b)
            | Expression::BitOr(a, b)
            | Expression::Eq(a, b)
            | Expression::Neq(a, b)
            | Expression::Le(a, b)
            | Expression::Gt(a, b)
            | Expression::Leq(a, b)
            | Expression::Geq(a, b)
            | Expression::And(a, b)
            | Expression::Or(a, b) => both(a, b, captures, live),
            Expression::Div(a, b) | Expression::Mod(a, b) => {
                int_literal(b).is_some_and(|k| k != 0) && both(a, b, captures, live)
            }
            Expression::Call {
                name,
                args: Some(args),
            } => {
                let pure_callee = matches!(
                    peel(name).1.as_ref(),
                    Expression::Identifier(f) if self.pure_fns.contains(*f)
                );
                pure_callee
                    && args.iter().all(|a| {
                        self.independent(a, index, acc, locals, consts, ints, captures, live)
                    })
            }
            _ => false,
        }
    }
}

/// One admissible statement of a loop-IPA body.
enum StmtForm<'a> {
    /// `i = i + k` / `i += k` / `i++` with `k > 0`.
    Step(i64),
    /// `acc = acc ⊕ expr` / `acc ⊕= expr`.
    Reduce {
        acc: &'a str,
        op: LoopReduceOp,
        expr: &'a Output<'a>,
    },
    /// `let name = init` — a loop-private temp.
    Local { name: &'a str, init: &'a Output<'a> },
    /// Pure `if` / `else`. Arms share the enclosing reduction.
    If { arms: Vec<IfArm<'a>> },
}

struct IfArm<'a> {
    cond: Option<&'a Output<'a>>,
    body: Vec<StmtForm<'a>>,
}

/// Classify a loop body. `allow_step` is false inside `if` (the latch stays outside).
fn classify_body<'a>(
    body: &'a Output<'a>,
    index: &str,
    allow_step: bool,
) -> Option<Vec<StmtForm<'a>>> {
    if let Some(items) = block_items(body) {
        items
            .iter()
            .map(|item| statement_form(peel(item), index, allow_step))
            .collect()
    } else {
        Some(vec![statement_form(peel(body), index, allow_step)?])
    }
}

fn collect_forms<'a>(
    forms: &'a [StmtForm<'a>],
    reduces: &mut Vec<(&'a str, LoopReduceOp, &'a Output<'a>)>,
    steps: &mut Vec<i64>,
    has_if: &mut bool,
) {
    for form in forms {
        match form {
            StmtForm::Step(k) => steps.push(*k),
            StmtForm::Reduce { acc, op, expr } => reduces.push((acc, *op, expr)),
            StmtForm::Local { .. } => {}
            StmtForm::If { arms } => {
                *has_if = true;
                for arm in arms {
                    collect_forms(&arm.body, reduces, steps, has_if);
                }
            }
        }
    }
}

/// Classify one body statement; `None` rejects the whole loop.
fn statement_form<'a>(item: &'a Output<'a>, index: &str, allow_step: bool) -> Option<StmtForm<'a>> {
    if let Some((name, init)) = let_binding(item) {
        return Some(StmtForm::Local { name, init });
    }
    match item.1.as_ref() {
        Expression::If(branches) => {
            let mut arms = Vec::new();
            for b in branches {
                let Expression::Branch(cond, body) = peel(b).1.as_ref() else {
                    return None;
                };
                arms.push(IfArm {
                    cond: cond.as_ref(),
                    body: classify_body(body, index, false)?,
                });
            }
            if arms.is_empty() {
                return None;
            }
            Some(StmtForm::If { arms })
        }
        Expression::Adjust {
            op: AdjustOp::Inc,
            target,
            ..
        } => (allow_step && ident_name(target)? == index).then_some(StmtForm::Step(1)),
        Expression::CompoundAssign(lhs, op, rhs) => {
            let name = ident_name(lhs)?;
            if name == index {
                let k = (*op == AssignOp::Add && allow_step)
                    .then(|| int_literal(rhs))
                    .flatten()
                    .filter(|k| *k > 0)?;
                return Some(StmtForm::Step(k));
            }
            let op = compound_reduce_op(*op)?;
            Some(StmtForm::Reduce {
                acc: name,
                op,
                expr: rhs,
            })
        }
        Expression::Assignment(lhs, rhs) => {
            let name = ident_name(lhs)?;
            if name == index {
                let k = allow_step.then(|| step_stride(rhs, index)).flatten()?;
                return Some(StmtForm::Step(k));
            }
            // `acc = acc ⊕ expr` or `acc = expr ⊕ acc` for commutative ops.
            let (op, expr) = match peel(rhs).1.as_ref() {
                Expression::Add(a, b) => commute_reduce(LoopReduceOp::Add, name, a, b)?,
                Expression::Mul(a, b) => commute_reduce(LoopReduceOp::Mul, name, a, b)?,
                Expression::Xor(a, b) => commute_reduce(LoopReduceOp::Xor, name, a, b)?,
                _ => return None,
            };
            Some(StmtForm::Reduce {
                acc: name,
                op,
                expr,
            })
        }
        _ => None,
    }
}

/// `i = i + k` or `i = k + i` with `k > 0`.
fn step_stride(rhs: &Output<'_>, index: &str) -> Option<i64> {
    let Expression::Add(a, b) = peel(rhs).1.as_ref() else {
        return None;
    };
    let k = if ident_name(a) == Some(index) {
        int_literal(b)
    } else if ident_name(b) == Some(index) {
        int_literal(a)
    } else {
        None
    }?;
    (k > 0).then_some(k)
}

fn compound_reduce_op(op: AssignOp) -> Option<LoopReduceOp> {
    match op {
        AssignOp::Add => Some(LoopReduceOp::Add),
        AssignOp::Mul => Some(LoopReduceOp::Mul),
        AssignOp::BitXor => Some(LoopReduceOp::Xor),
        _ => None,
    }
}

/// Acc must be one operand of a commutative `⊕`; the other is the contribution.
fn commute_reduce<'a>(
    op: LoopReduceOp,
    acc: &str,
    a: &'a Output<'a>,
    b: &'a Output<'a>,
) -> Option<(LoopReduceOp, &'a Output<'a>)> {
    if ident_name(a) == Some(acc) {
        Some((op, b))
    } else if ident_name(b) == Some(acc) {
        Some((op, a))
    } else {
        None
    }
}

/// `i < K` / `i <= K`. The bound is a const int or an int local.
///
/// Returns `(index, index ptr, end local, end const, inclusive)`.
fn counted_bound(
    cond: &Output<'_>,
    consts: &ConstLocals,
    ints: &HashSet<String>,
) -> Option<(String, usize, Option<String>, i64, bool)> {
    let cond = peel(cond);
    let (lhs, rhs, inclusive) = match cond.1.as_ref() {
        Expression::Le(a, b) => (a, b, false),
        Expression::Leq(a, b) => (a, b, true),
        _ => return None,
    };
    let lhs = peel(lhs);
    let index = ident_name(lhs)?;
    let (local, konst) = int_endpoint(rhs, consts, ints)?;
    Some((
        index.to_string(),
        std::ptr::from_ref(lhs) as *const Output<'_> as usize,
        local,
        konst,
        inclusive,
    ))
}

/// Integer range on a for-in iterable, half-open when both ends are const.
///
/// Returns `(begin, end, begin local, end local, end bias)`.
type CountedRange = (i64, i64, Option<String>, Option<String>, i64);

fn counted_range(
    iterable: &Output<'_>,
    consts: &ConstLocals,
    ints: &HashSet<String>,
) -> Option<CountedRange> {
    if let Some(ConstVal::Range { begin, end }) = const_val(iterable, consts) {
        return Some((begin, end, None, None, 0));
    }
    let expr = peel(iterable);
    let Expression::Range {
        start,
        end,
        inclusive,
    } = expr.1.as_ref()
    else {
        return None;
    };
    let (begin_local, begin) = int_endpoint(start, consts, ints)?;
    let (end_local, end) = int_endpoint(end, consts, ints)?;
    if begin_local.is_none() && end_local.is_none() {
        let end = if *inclusive { end.checked_add(1)? } else { end };
        return Some((begin, end, None, None, 0));
    }
    let bias = if *inclusive { 1 } else { 0 };
    Some((begin, end, begin_local, end_local, bias))
}

/// Const int, or an int-typed local. `(local name, const value)`.
fn int_endpoint(
    expr: &Output<'_>,
    consts: &ConstLocals,
    ints: &HashSet<String>,
) -> Option<(Option<String>, i64)> {
    if let Some(k) = const_int(expr, consts) {
        return Some((None, k));
    }
    let name = ident_name(peel(expr))?;
    ints.contains(name).then(|| (Some(name.to_string()), 0))
}

fn const_val(expr: &Output<'_>, consts: &ConstLocals) -> Option<ConstVal> {
    if let Some(k) = const_int(expr, consts) {
        return Some(ConstVal::Int(k));
    }
    let expr = peel(expr);
    match expr.1.as_ref() {
        Expression::Range {
            start,
            end,
            inclusive,
        } => {
            let begin = const_int(start, consts)?;
            let end = const_int(end, consts)?;
            let end = if *inclusive { end.checked_add(1)? } else { end };
            Some(ConstVal::Range { begin, end })
        }
        Expression::Identifier(n) => consts.get(*n).copied(),
        _ => None,
    }
}

fn const_int(expr: &Output<'_>, consts: &ConstLocals) -> Option<i64> {
    match peel(expr).1.as_ref() {
        Expression::Integer(k) => Some(*k),
        Expression::Identifier(n) => match consts.get(*n) {
            Some(ConstVal::Int(k)) => Some(*k),
            _ => None,
        },
        _ => None,
    }
}

fn note_binding_effects(item: &Output<'_>, consts: &mut ConstLocals, ints: &mut HashSet<String>) {
    if let Some((lhs, rhs)) = assign_pair(item) {
        if let Some(name) = ident_name(lhs) {
            consts.remove(name);
            if is_int_expr(rhs, ints) {
                ints.insert(name.to_string());
            } else {
                ints.remove(name);
            }
        }
    } else {
        for name in assigned_names(item) {
            consts.remove(&name);
            ints.remove(&name);
        }
    }
    if let Some((name, init)) = let_binding(item) {
        match const_val(init, consts) {
            Some(v) => {
                consts.insert(name.to_string(), v);
            }
            None => {
                consts.remove(name);
            }
        }
        if is_int_expr(init, ints) {
            ints.insert(name.to_string());
        } else {
            ints.remove(name);
        }
    }
}

fn assign_pair<'a>(item: &'a Output<'a>) -> Option<(&'a Output<'a>, &'a Output<'a>)> {
    match peel(item).1.as_ref() {
        Expression::Assignment(lhs, rhs) | Expression::CompoundAssign(lhs, _, rhs) => {
            Some((lhs, rhs))
        }
        _ => None,
    }
}

fn is_int_expr(expr: &Output<'_>, ints: &HashSet<String>) -> bool {
    let expr = peel(expr);
    match expr.1.as_ref() {
        Expression::Integer(_) => true,
        Expression::Identifier(n) => ints.contains(*n),
        Expression::Negate(a) | Expression::Positive(a) => is_int_expr(a, ints),
        Expression::Add(a, b)
        | Expression::Sub(a, b)
        | Expression::Mul(a, b)
        | Expression::Shl(a, b)
        | Expression::Shr(a, b)
        | Expression::Xor(a, b)
        | Expression::BitAnd(a, b)
        | Expression::BitOr(a, b) => is_int_expr(a, ints) && is_int_expr(b, ints),
        _ => false,
    }
}

fn int_param_names(args: &Output<'_>) -> HashSet<String> {
    let mut out = HashSet::new();
    let items = match args.1.as_ref() {
        Expression::Fragment(items) | Expression::Block(items) => items.as_slice(),
        _ => return out,
    };
    for item in items {
        let Expression::Argument { name, ty, .. } = peel(item).1.as_ref() else {
            continue;
        };
        let ty_name = ty.as_ref().and_then(|t| match peel(t).1.as_ref() {
            Expression::Type(n) | Expression::Identifier(n) => Some(*n),
            _ => None,
        });
        if matches!(ty_name, Some("int") | Some("byte")) {
            out.insert((*name).to_string());
        }
    }
    out
}

/// Statement list of a loop body, or `None` for a single-expression body.
fn block_items<'a>(body: &'a Output<'a>) -> Option<&'a [Output<'a>]> {
    match peel(body).1.as_ref() {
        Expression::Block(items) | Expression::Fragment(items) | Expression::Program(items) => {
            Some(items.as_slice())
        }
        _ => None,
    }
}

/// Local names written anywhere inside `ast`.
fn assigned_names(ast: &Output<'_>) -> HashSet<String> {
    let mut out = HashSet::new();
    collect_assigned(ast, &mut out);
    out
}

fn collect_assigned(ast: &Output<'_>, out: &mut HashSet<String>) {
    match ast.1.as_ref() {
        Expression::Assignment(lhs, rhs) | Expression::CompoundAssign(lhs, _, rhs) => {
            if let Some(n) = ident_name(lhs) {
                out.insert(n.to_string());
            }
            collect_assigned(lhs, out);
            collect_assigned(rhs, out);
        }
        Expression::Adjust { target, .. } => {
            if let Some(n) = ident_name(target) {
                out.insert(n.to_string());
            }
            collect_assigned(target, out);
        }
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::List(items)
        | Expression::Array(items)
        | Expression::Tuple(items)
        | Expression::If(items) => {
            for item in items {
                collect_assigned(item, out);
            }
        }
        Expression::Module(_, inner)
        | Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner)
        | Expression::Return(inner)
        | Expression::ImplicitReturn(inner)
        | Expression::Raise(inner)
        | Expression::Try(inner)
        | Expression::Negate(inner)
        | Expression::Positive(inner)
        | Expression::Not(inner)
        | Expression::LogicalNot(inner)
        | Expression::Cast(inner, _)
        | Expression::Readonly(inner)
        | Expression::Variable(_, Some(inner))
        | Expression::Method(_, inner)
        | Expression::Member(inner) => collect_assigned(inner, out),
        Expression::Add(a, b)
        | Expression::Sub(a, b)
        | Expression::Mul(a, b)
        | Expression::Div(a, b)
        | Expression::Mod(a, b)
        | Expression::Pow(a, b)
        | Expression::Shl(a, b)
        | Expression::Shr(a, b)
        | Expression::Xor(a, b)
        | Expression::And(a, b)
        | Expression::BitAnd(a, b)
        | Expression::Or(a, b)
        | Expression::BitOr(a, b)
        | Expression::Eq(a, b)
        | Expression::Neq(a, b)
        | Expression::Le(a, b)
        | Expression::Gt(a, b)
        | Expression::Leq(a, b)
        | Expression::Geq(a, b)
        | Expression::Coalesce(a, b) => {
            collect_assigned(a, out);
            collect_assigned(b, out);
        }
        Expression::Call { name, args } => {
            collect_assigned(name, out);
            for a in args.iter().flatten() {
                collect_assigned(a, out);
            }
        }
        Expression::Branch(cond, body) => {
            if let Some(c) = cond {
                collect_assigned(c, out);
            }
            collect_assigned(body, out);
        }
        Expression::Match { scrutinee, arms } => {
            collect_assigned(scrutinee, out);
            for arm in arms {
                collect_assigned(&arm.body, out);
            }
        }
        Expression::Loop { iterable, body, .. } => {
            collect_assigned(iterable, out);
            collect_assigned(body, out);
        }
        Expression::Function {
            body: Some(body), ..
        }
        | Expression::Lambda { body, .. }
        | Expression::Defer { body, .. } => collect_assigned(body, out),
        Expression::Implementation { methods, .. } => {
            for m in methods {
                collect_assigned(m, out);
            }
        }
        Expression::LetDestructure { rhs, .. } => collect_assigned(rhs, out),
        _ => {}
    }
}

/// `let name = init` in either parsed form: a two-element `Fragment`
/// (`Variable(name, None)` followed by the initializer) or `Variable` with an
/// inline initializer.
fn let_binding<'a>(item: &'a Output<'a>) -> Option<(&'a str, &'a Output<'a>)> {
    match peel(item).1.as_ref() {
        Expression::Variable(name, Some(init)) => Some((name, init)),
        Expression::Fragment(items) => match items.as_slice() {
            [binder, init] => match peel(binder).1.as_ref() {
                Expression::Variable(name, None) => Some((name, init)),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

fn ident_name<'a>(expr: &'a Output<'a>) -> Option<&'a str> {
    match peel(expr).1.as_ref() {
        Expression::Identifier(n) => Some(*n),
        _ => None,
    }
}

fn int_literal(expr: &Output<'_>) -> Option<i64> {
    match peel(expr).1.as_ref() {
        Expression::Integer(k) => Some(*k),
        _ => None,
    }
}

fn peel<'a>(expr: &'a Output<'a>) -> &'a Output<'a> {
    match expr.1.as_ref() {
        Expression::Expr(inner)
        | Expression::Group(inner)
        | Expression::Statement(inner)
        | Expression::ExprStatement(inner) => peel(inner),
        Expression::Fragment(items) if items.len() == 1 => peel(&items[0]),
        _ => expr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typechecking::purity::analyze_pure_fns;
    use parser::Pratt;

    fn sites_of(src: &str) -> Vec<LoopParSite> {
        let owned = Box::leak(src.to_string().into_boxed_str());
        let ast = Pratt::default().parse(owned).expect("parse");
        let pure = analyze_pure_fns(&ast);
        let mut sites: Vec<LoopParSite> =
            analyze_loop_par_sites(&ast, &pure).into_values().collect();
        sites.sort_by_key(|a| a.begin);
        sites
    }

    fn one_site(src: &str) -> LoopParSite {
        let sites = sites_of(src);
        assert_eq!(sites.len(), 1, "expected exactly one site: {sites:?}");
        sites.into_iter().next().unwrap()
    }

    /// `fn main` wrapper with a threshold-beating trip count.
    fn program(body: &str) -> String {
        format!(
            r#"
fn sq(int i) -> int {{ return i * i; }}
fn main() {{
{body}
}}
"#
        )
    }

    #[test]
    fn detects_add_reduction_over_pure_call() {
        let site = one_site(&program(
            r#"
    let acc = 0;
    let i = 0;
    while i < 100 {
        acc = acc + sq(i);
        i = i + 1;
    }
"#,
        ));
        assert_eq!(site.index, "i");
        assert_eq!(site.acc, "acc");
        assert_eq!((site.begin, site.end), (0, 100));
        assert_eq!(site.op, LoopReduceOp::Add);
        assert_eq!(site.trip_count(), 100);
        assert_eq!(site.midpoint(), 50);
        assert!(!site.implicit_step);
    }

    #[test]
    fn detects_literal_for_range() {
        let site = one_site(&program(
            r#"
    let acc = 0;
    for x in 0..100 {
        acc = acc + sq(x);
    }
"#,
        ));
        assert_eq!(site.index, "x");
        assert_eq!(site.acc, "acc");
        assert_eq!((site.begin, site.end), (0, 100));
        assert_eq!(site.op, LoopReduceOp::Add);
        assert!(site.implicit_step);
        assert_eq!(site.trip_count(), 100);
        assert_eq!(site.midpoint(), 50);
    }

    #[test]
    fn detects_inclusive_for_range() {
        let site = one_site(&program(
            r#"
    let acc = 0;
    for x in 1..=60 {
        acc += sq(x);
    }
"#,
        ));
        assert_eq!((site.begin, site.end), (1, 61));
        assert!(site.implicit_step);
        assert_eq!(site.trip_count(), 60);
    }

    #[test]
    fn detects_const_range_local() {
        let site = one_site(&program(
            r#"
    let r = 0..100;
    let acc = 0;
    for x in r {
        acc = acc + sq(x);
    }
"#,
        ));
        assert_eq!((site.begin, site.end), (0, 100));
        assert!(site.implicit_step);
    }

    #[test]
    fn inclusive_bound_normalizes_half_open() {
        let site = one_site(&program(
            r#"
    let acc = 0;
    let i = 1;
    while i <= 60 {
        acc += sq(i);
        i += 1;
    }
"#,
        ));
        assert_eq!((site.begin, site.end), (1, 61));
        assert_eq!(site.trip_count(), 60);
        assert_eq!(site.midpoint(), 31);
    }

    #[test]
    fn detects_mul_reduction_and_identity() {
        let site = one_site(&program(
            r#"
    let prod = 1;
    let i = 1;
    while i < 40 {
        prod = prod * i;
        i = i + 1;
    }
"#,
        ));
        assert_eq!(site.op, LoopReduceOp::Mul);
        assert_eq!(site.acc, "prod");
        assert_eq!(LoopReduceOp::Mul.identity(), 1);
        assert_eq!(LoopReduceOp::Add.identity(), 0);
    }

    #[test]
    fn admits_loop_private_temps() {
        let site = one_site(&program(
            r#"
    let acc = 0;
    let i = 0;
    while i < 100 {
        let x = sq(i);
        let y = x + i;
        acc = acc + y;
        i = i + 1;
    }
"#,
        ));
        assert_eq!(site.trip_count(), 100);
    }

    #[test]
    fn rejects_trip_count_at_threshold() {
        let t = par_loop_grain();
        assert!(
            sites_of(&program(&format!(
                r#"
    let acc = 0;
    let i = 0;
    while i < {t} {{
        acc = acc + sq(i);
        i = i + 1;
    }}
"#
            )))
            .is_empty(),
            "trip count == threshold must stay sequential"
        );
        assert!(
            sites_of(&program(&format!(
                r#"
    let acc = 0;
    for x in 0..{t} {{
        acc = acc + sq(x);
    }}
"#
            )))
            .is_empty(),
            "for-range trip count == threshold must stay sequential"
        );
    }

    #[test]
    fn admits_dynamic_parameter_bounds() {
        let while_site = one_site(
            r#"
fn sq(int i) -> int { return i * i; }
fn run(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        acc = acc + sq(i);
        i = i + 1;
    }
    return acc;
}
fn main() { return; }
"#,
        );
        assert!(while_site.is_dynamic());
        assert_eq!(while_site.end_local.as_deref(), Some("n"));
        let for_site = one_site(
            r#"
fn sq(int i) -> int { return i * i; }
fn run(int n) -> int {
    let acc = 0;
    for x in 0..n {
        acc = acc + sq(x);
    }
    return acc;
}
fn main() { return; }
"#,
        );
        assert!(for_site.is_dynamic());
        assert_eq!(for_site.end_local.as_deref(), Some("n"));
        assert!(for_site.implicit_step);
    }

    #[test]
    fn rejects_impure_body_call() {
        assert!(
            sites_of(
                r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn shout(int i) -> int {
    write(stdout(), to_bytes(format("%i", i)));
    return i;
}
fn main() {
    let acc = 0;
    let i = 0;
    while i < 100 {
        acc = acc + shout(i);
        i = i + 1;
    }
}
"#
            )
            .is_empty(),
            "an impure call is observable per iteration"
        );
    }

    /// `acc` on the right of its own reduction is a loop-carried dependence the
    /// chunk split would reorder.
    #[test]
    fn rejects_accumulator_read_in_reduction_operand() {
        assert!(
            sites_of(&program(
                r#"
    let acc = 0;
    let i = 0;
    while i < 100 {
        acc = acc + acc;
        i = i + 1;
    }
"#
            ))
            .is_empty(),
            "reduction operand must not read the accumulator"
        );
    }

    #[test]
    fn rejects_second_reduction() {
        assert!(
            sites_of(&program(
                r#"
    let a = 0;
    let b = 0;
    let i = 0;
    while i < 100 {
        a = a + i;
        b = b + i;
        i = i + 1;
    }
"#
            ))
            .is_empty(),
            "only a single reduction is representable"
        );
    }

    #[test]
    fn admits_int_parameter_capture() {
        let site = one_site(
            r#"
fn sq(int i) -> int { return i * i; }
fn run(int k) -> int {
    let acc = 0;
    let i = 0;
    while i < 100 {
        acc = acc + k;
        i = i + 1;
    }
    return acc;
}
fn main() { return; }
"#,
        );
        assert_eq!(site.live_captures, vec!["k".to_string()]);
        assert_eq!(site.trip_count(), 100);
    }

    #[test]
    fn rejects_non_int_capture() {
        assert!(
            sites_of(
                r#"
fn sq(int i) -> int { return i * i; }
fn run(string s) -> int {
    let acc = 0;
    let i = 0;
    while i < 100 {
        acc = acc + sq(i);
        let t = s;
        i = i + 1;
    }
    return acc;
}
fn main() { return; }
"#
            )
            .is_empty(),
            "a string local is not an int capture"
        );
    }

    #[test]
    fn admits_enclosing_const_int() {
        let site = one_site(&program(
            r#"
    let scale = 3;
    let acc = 0;
    let i = 0;
    while i < 100 {
        acc = acc + scale * sq(i);
        i = i + 1;
    }
"#,
        ));
        assert_eq!(site.captures, vec![("scale".to_string(), 3)]);
        assert_eq!(site.trip_count(), 100);
    }

    #[test]
    fn detects_xor_reduction() {
        let site = one_site(&program(
            r#"
    let acc = 0;
    let i = 0;
    while i < 40 {
        acc = acc ^ sq(i);
        i = i + 1;
    }
"#,
        ));
        assert_eq!(site.op, LoopReduceOp::Xor);
        assert_eq!(LoopReduceOp::Xor.identity(), 0);
    }

    #[test]
    fn admits_commuted_reduction() {
        let site = one_site(&program(
            r#"
    let acc = 0;
    let i = 0;
    while i < 100 {
        acc = sq(i) + acc;
        i = i + 1;
    }
"#,
        ));
        assert_eq!(site.op, LoopReduceOp::Add);
        assert_eq!(site.trip_count(), 100);
    }

    #[test]
    fn admits_pure_branch_on_one_reduction() {
        let site = one_site(&program(
            r#"
    let acc = 0;
    let i = 0;
    while i < 100 {
        if i > 3 { acc = acc + 1; }
        i = i + 1;
    }
"#,
        ));
        assert_eq!(site.op, LoopReduceOp::Add);
        assert_eq!(site.trip_count(), 100);
    }

    #[test]
    fn rejects_mixed_ops_across_branch() {
        assert!(
            sites_of(&program(
                r#"
    let acc = 0;
    let i = 0;
    while i < 100 {
        if i > 3 { acc = acc + 1; } else { acc = acc * 2; }
        i = i + 1;
    }
"#
            ))
            .is_empty(),
            "both arms must fold with the same operator"
        );
    }

    #[test]
    fn admits_dynamic_end_and_const_stride() {
        let site = one_site(
            r#"
fn sq(int i) -> int { return i * i; }
fn run(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        acc = acc + sq(i);
        i = i + 1;
    }
    return acc;
}
fn main() { return; }
"#,
        );
        assert!(site.is_dynamic());
        assert_eq!(site.end_local.as_deref(), Some("n"));
        assert_eq!(site.stride, 1);
        assert_eq!(site.trip_count(), 0);
    }

    #[test]
    fn chunks_a_unit_stride_range_into_at_most_four() {
        let site = one_site(&program(
            r#"
    let acc = 0;
    let i = 0;
    while i < 100 {
        acc = acc + sq(i);
        i = i + 1;
    }
"#,
        ));
        assert_eq!(site.chunk_bounds(20), Some(vec![0, 25, 50, 75, 100]));
        assert_eq!(site.midpoint(), 50);
    }

    #[test]
    fn admits_positive_stride() {
        let site = one_site(&program(
            r#"
    let acc = 0;
    let i = 0;
    while i < 80 {
        acc = acc + i;
        i = i + 2;
    }
"#,
        ));
        assert_eq!(site.stride, 2);
        assert_eq!(site.trip_count(), 40);
        assert_eq!(site.final_index(), 80);
        assert_eq!(site.chunk_bounds(20), Some(vec![0, 40, 80]));
    }

    #[test]
    fn rejects_index_store_in_body() {
        assert!(
            sites_of(&program(
                r#"
    let acc = 0;
    let i = 0;
    let buf = [0, 0];
    while i < 100 {
        buf[0] = i;
        acc = acc + i;
        i = i + 1;
    }
"#
            ))
            .is_empty(),
            "a shared write is not an independent arm"
        );
    }

    #[test]
    fn rejects_missing_or_duplicated_step() {
        assert!(
            sites_of(&program(
                r#"
    let acc = 0;
    let i = 0;
    while i < 100 {
        acc = acc + i;
        i = i + 1;
        i = i + 1;
    }
"#
            ))
            .is_empty(),
            "two steps mean the induction range is not [begin, end)"
        );
    }

    /// A const local's binding does not describe every visit to a program point
    /// inside an enclosing loop, so the inner range would be wrong.
    #[test]
    fn rejects_counted_loop_nested_in_another_loop() {
        assert!(
            sites_of(&program(
                r#"
    let acc = 0;
    let i = 0;
    let outer = 0;
    while outer < 4 {
        while i < 100 {
            acc = acc + sq(i);
            i = i + 1;
        }
        outer = outer + 1;
    }
"#
            ))
            .is_empty(),
            "an induction variable carried across an outer loop is not const"
        );
    }

    /// Reassigning the accumulator between its `let` and the loop drops the
    /// locality proof.
    #[test]
    fn rejects_accumulator_clobbered_before_loop() {
        assert!(
            sites_of(&program(
                r#"
    let acc = 0;
    let i = 0;
    acc = sq(3);
    while i < 100 {
        acc = acc + sq(i);
        i = i + 1;
    }
"#
            ))
            .is_empty(),
            "accumulator must still be provably a plain local at loop entry"
        );
    }
}
