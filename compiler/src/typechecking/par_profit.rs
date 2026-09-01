//! Static shape + profitability analysis for Independent Parallel Arms (IPA).
//!
//! A *fork site* is an expression whose operands are two or more mutually
//! independent **pure** calls — self-recursion is common but not required:
//! `f(a) ⊕ f(b)`, `h(a) + h(b)`, `E::V(f(a), f(b))`, `(f(a), f(b))`, the
//! tak-style `f(f(a), f(b), f(c))`, or `g(f(a), f(b))`. Arms are described
//! structurally ([`ArgForm`]) rather than by function allowlists. Constant call
//! sites whose estimated **work** ([`par_work_units`]) exceeds
//! [`par_cost_threshold`] rewrite to specialized nullary clones that always fork
//! (fully static, no runtime threshold checks).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use parser::ast::{EnumConstructPayload, Expression, Output, Pattern};

/// Compile-time fork threshold (`COIL_PAR_THRESHOLD`, default 20).
pub fn par_cost_threshold() -> i64 {
    static T: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *T.get_or_init(|| {
        std::env::var("COIL_PAR_THRESHOLD")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20)
    })
}

/// Binary op used at a [`ParCombine::BinOp`] fork site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParBinOp {
    Add,
    Sub,
    Mul,
}

/// One argument of a self-call arm, expressed in terms of the enclosing
/// function's parameters so child arg vectors can be derived statically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArgForm {
    /// Literal integer.
    Const(i64),
    /// Parameter forwarded unchanged (`x`).
    Param(usize),
    /// `param - sub` with `sub > 0`; requires an int-like parameter.
    ParamMinus { param: usize, sub: i64 },
}

/// One independent arm of a fork site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParArm {
    /// Call to a pure function (often self); one [`ArgForm`] per callee parameter.
    Call { callee: String, args: Vec<ArgForm> },
}

/// How arm results are recombined once every arm has been joined.
///
/// The combine always consumes the arm results positionally and in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParCombine {
    /// `arm0 ⊕ arm1` (exactly two arms).
    BinOp(ParBinOp),
    /// Rebuild by calling the enclosing fn with the arm results as args (tak-style).
    SelfCall,
    /// Call some other pure fn with the arm results as args.
    ApplyCall { fn_name: String },
    /// `(arm0, arm1, …)` tuple pack.
    Tuple,
    /// `EnumName::Variant(arm0, arm1, …)`.
    EnumCtor {
        enum_name: String,
        variant_name: String,
    },
}

/// Integer comparison in a fork-site [`ParGuard`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Lt,
    Leq,
    Gt,
    Geq,
    Eq,
    Neq,
}

/// A condition that must hold for control to reach the fork site.
///
/// A specialized clone skips the function's base case entirely, so it may only
/// be entered at arg vectors that actually reach the fork. [`Self::Opaque`]
/// stands for any condition the analysis cannot evaluate and never holds, so
/// unrecognized guards simply disable specialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParGuard {
    Cmp {
        lhs: ArgForm,
        op: CmpOp,
        rhs: ArgForm,
        /// Whether the comparison must be true or false at the fork site.
        expect: bool,
    },
    Opaque,
}

/// A pure function's primary parallelizable fork site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParForkSite {
    pub fn_name: String,
    /// Declared parameter count of the enclosing function (for specialization keys).
    pub param_count: usize,
    /// Independent pure calls (at least two).
    pub arms: Vec<ParArm>,
    pub combine: ParCombine,
    /// Conditions, all of which must hold for the fork site to be reached.
    pub guards: Vec<ParGuard>,
}

/// True when every guard holds for a concrete arg vector.
pub fn guards_hold(guards: &[ParGuard], args: &[i64]) -> bool {
    guards.iter().all(|g| match g {
        ParGuard::Opaque => false,
        ParGuard::Cmp {
            lhs,
            op,
            rhs,
            expect,
        } => {
            let (Some(l), Some(r)) = (eval_arg_form(lhs, args), eval_arg_form(rhs, args)) else {
                return false;
            };
            let holds = match op {
                CmpOp::Lt => l < r,
                CmpOp::Leq => l <= r,
                CmpOp::Gt => l > r,
                CmpOp::Geq => l >= r,
                CmpOp::Eq => l == r,
                CmpOp::Neq => l != r,
            };
            holds == *expect
        }
    })
}

// ---------------------------------------------------------------------------
// Structural work score
// ---------------------------------------------------------------------------

/// Fork-site nodes in the tree of `fib(n) = fib(n-1) + fib(n-2)`, the shape the
/// threshold is calibrated on: `W(k) = 1 + W(k-1) + W(k-2)`, `W(k <= 1) = 0`,
/// which closes to `Fib(n + 1) - 1`. Saturates instead of overflowing.
fn fib_tree_nodes(n: i64) -> i64 {
    if n <= 1 {
        return 0;
    }
    let (mut prev, mut cur) = (1i64, 1i64); // Fib(1), Fib(2)
    for _ in 2..=n.min(FIB_UNITS_MAX) {
        let next = prev.saturating_add(cur);
        prev = cur;
        cur = next;
    }
    cur.saturating_sub(1)
}

/// `Fib(n + 1)` overflows `i64` past this, so units saturate here.
const FIB_UNITS_MAX: i64 = 91;

/// Node counts back into threshold units: the smallest `n` whose `fib(n)` tree
/// is at least this big. Exact inverse of [`fib_tree_nodes`], so a fib-shaped
/// site at `n` scores exactly `n`.
#[cfg(test)]
fn fib_tree_units(nodes: i64) -> i64 {
    let mut n = 0;
    while n < FIB_UNITS_MAX && fib_tree_nodes(n) < nodes {
        n += 1;
    }
    n
}

/// Recursion depth at which the estimator stops descending and calls it a leaf.
const WORK_MAX_DEPTH: u32 = 256;

/// Distinct arg vectors the estimator will memoize before giving up.
const WORK_MEMO_CAP: usize = 1 << 14;

/// Bounded structural estimate of the work below a fork site.
///
/// Counts the fork-site nodes a concrete arg vector reaches through the arms'
/// [`ArgForm`] transforms, pruning children that miss the site's guards — those
/// are base cases and do no forkable work. Every imprecision (opaque callees, a
/// `SelfCall` combine's re-entry on joined values, the caps below) resolves
/// *downwards*, so the count is a lower bound: unknown structure only refuses.
struct WorkEstimate<'a> {
    sites: &'a HashMap<String, ParForkSite>,
    /// Counting past the cutoff cannot change the verdict, so totals stop here.
    cap: i64,
    memo: HashMap<(&'a str, Vec<i64>), i64>,
}

impl<'a> WorkEstimate<'a> {
    fn new(sites: &'a HashMap<String, ParForkSite>) -> Self {
        Self {
            sites,
            cap: fib_tree_nodes(par_cost_threshold()).saturating_add(1),
            memo: HashMap::new(),
        }
    }

    /// Work below `fn_name(args)` in threshold units, saturating one unit past
    /// the cutoff (scores at or below it are exact).
    #[cfg(test)]
    fn units(&mut self, fn_name: &str, args: &[i64]) -> i64 {
        let nodes = self.nodes(fn_name, args, 0);
        fib_tree_units(nodes)
    }

    fn worth_parallel(&mut self, fn_name: &str, args: &[i64]) -> bool {
        self.nodes(fn_name, args, 0) > fib_tree_nodes(par_cost_threshold())
    }

    fn nodes(&mut self, fn_name: &str, args: &[i64], depth: u32) -> i64 {
        let sites = self.sites;
        let Some(site) = sites.get(fn_name) else {
            return 0;
        };
        // Negative args are base-case territory, and a clone is only entered
        // where the guards let control reach the fork.
        if args.len() != site.param_count
            || args.iter().any(|a| *a < 0)
            || !guards_hold(&site.guards, args)
        {
            return 0;
        }
        let key = (site.fn_name.as_str(), args.to_vec());
        if let Some(&hit) = self.memo.get(&key) {
            return hit;
        }
        if depth >= WORK_MAX_DEPTH || self.memo.len() >= WORK_MEMO_CAP {
            return 0;
        }
        // `Const` arg forms can raise a component, so the arg graph may cycle;
        // the placeholder makes a re-entrant vector a leaf.
        self.memo.insert(key.clone(), 0);
        let mut total: i64 = 1;
        for arm in &site.arms {
            let Some(child) = eval_arm_args(arm, args) else {
                continue;
            };
            total = total.saturating_add(self.nodes(arm_callee(arm), &child, depth + 1));
            if total >= self.cap {
                total = self.cap;
                break;
            }
        }
        self.memo.insert(key, total);
        total
    }
}

/// Structural work below `fn_name(args)`'s fork site, in [`par_cost_threshold`]
/// units (a fib-shaped site at `n` scores `n`). Saturates at `threshold + 1`.
#[cfg(test)]
pub fn par_work_units(sites: &HashMap<String, ParForkSite>, fn_name: &str, args: &[i64]) -> i64 {
    WorkEstimate::new(sites).units(fn_name, args)
}

/// True when `fn_name(args)` carries more work than [`par_cost_threshold`].
pub fn args_worth_parallel(
    sites: &HashMap<String, ParForkSite>,
    fn_name: &str,
    args: &[i64],
) -> bool {
    WorkEstimate::new(sites).worth_parallel(fn_name, args)
}

/// Specialized nullary entry name for `fn_name` at concrete `args`.
///
/// `("fib", &[22])` → `__coil_par_fib_22`; `("tak", &[18, 12, 6])` →
/// `__coil_par_tak_18_12_6`.
pub fn par_specialization_name(fn_name: &str, args: &[i64]) -> String {
    let mut out = format!("__coil_par_{fn_name}");
    for a in args {
        out.push('_');
        out.push_str(&a.to_string());
    }
    out
}

/// Concrete child args for `arm` given the enclosing call's `parent_args`.
pub fn eval_arm_args(arm: &ParArm, parent_args: &[i64]) -> Option<Vec<i64>> {
    let ParArm::Call { args, .. } = arm;
    args.iter().map(|f| eval_arg_form(f, parent_args)).collect()
}

/// Callee bare name for a [`ParArm::Call`].
pub fn arm_callee(arm: &ParArm) -> &str {
    let ParArm::Call { callee, .. } = arm;
    callee
}

fn eval_arg_form(form: &ArgForm, parent_args: &[i64]) -> Option<i64> {
    match form {
        ArgForm::Const(k) => Some(*k),
        ArgForm::Param(i) => parent_args.get(*i).copied(),
        ArgForm::ParamMinus { param, sub } => parent_args.get(*param).map(|v| v - sub),
    }
}

/// Collect the primary fork site of every pure function that has one.
///
/// One site per function: the first profitable clear-path site found walking
/// the body, preferring return-path sites without opaque guards.
pub fn analyze_par_fork_sites(
    ast: &Output<'_>,
    pure_fns: &HashSet<String>,
) -> HashMap<String, ParForkSite> {
    let mut out = HashMap::new();
    collect_sites(ast, pure_fns, &mut out);
    out
}

/// Per-function cap on demanded specializations.
///
/// Each entry becomes an emitted clone that always forks, so multi-arg sites
/// (whose closure grows combinatorially) need a code-size and spawn budget.
/// Levels dropped by the budget simply stay on the sequential original.
const PAR_SPEC_BUDGET: usize = 64;

/// Constant call-site arg vectors for fork-site functions, closed under the
/// arm transforms so every specialization a clone can reach also exists.
///
/// The closure is breadth-first (shallowest levels are the profitable ones)
/// and bounded by [`PAR_SPEC_BUDGET`].
pub fn collect_par_specialization_args(
    ast: &Output<'_>,
    sites: &HashMap<String, ParForkSite>,
) -> HashMap<String, BTreeSet<Vec<i64>>> {
    let mut demanded: HashMap<String, BTreeSet<Vec<i64>>> = HashMap::new();
    let mut work = WorkEstimate::new(sites);
    collect_const_calls(ast, &mut work, &mut demanded);
    for (name, set) in demanded.iter_mut() {
        let Some(site) = sites.get(name) else {
            continue;
        };
        // A clone has no base case, so it may only stand in for arg vectors
        // that actually reach the fork site.
        set.retain(|args| guards_hold(&site.guards, args));
        let mut queue: VecDeque<Vec<i64>> = set.iter().cloned().collect();
        let mut seen: HashSet<Vec<i64>> = set.iter().cloned().collect();
        while let Some(cur) = queue.pop_front() {
            if seen.len() >= PAR_SPEC_BUDGET {
                break;
            }
            for arm in &site.arms {
                let Some(child) = eval_arm_args(arm, &cur) else {
                    continue;
                };
                // Negative args are base-case territory, and a child that no
                // longer carries threshold work stays on the sequential
                // original — which is also what bounds the closure.
                if child.iter().any(|a| *a < 0)
                    || !work.worth_parallel(arm_callee(arm), &child)
                    || !guards_hold(&site.guards, &child)
                {
                    continue;
                }
                if seen.len() >= PAR_SPEC_BUDGET {
                    break;
                }
                if seen.insert(child.clone()) {
                    queue.push_back(child);
                }
            }
        }
        *set = seen.into_iter().collect();
    }
    demanded.retain(|name, set| {
        set.retain(|args| work.worth_parallel(name, args));
        !set.is_empty()
    });
    demanded
}

// ---------------------------------------------------------------------------
// Fork-site detection
// ---------------------------------------------------------------------------

/// Parameters of the function currently being scanned.
struct FnCtx<'a> {
    fn_name: &'a str,
    param_names: Vec<String>,
    /// `param - k` forms are only meaningful for int-like parameters.
    param_int_like: Vec<bool>,
    /// Pure callees allowed as fork arms (includes the enclosing fn when pure).
    pure_fns: &'a HashSet<String>,
}

impl FnCtx<'_> {
    fn param_index(&self, name: &str) -> Option<usize> {
        self.param_names.iter().position(|p| p == name)
    }

    fn is_pure_callee(&self, name: &str) -> bool {
        self.pure_fns.contains(name)
    }
}

fn collect_sites(
    ast: &Output<'_>,
    pure_fns: &HashSet<String>,
    out: &mut HashMap<String, ParForkSite>,
) {
    match ast.1.as_ref() {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            for item in items {
                collect_sites(item, pure_fns, out);
            }
        }
        Expression::Module(_, body) => collect_sites(body, pure_fns, out),
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner) => collect_sites(inner, pure_fns, out),
        Expression::Function {
            name,
            args,
            body: Some(body),
            ..
        } if pure_fns.contains(*name) => {
            if let Some(site) = detect_fork_site(name, args, body, pure_fns) {
                out.insert((*name).to_string(), site);
            }
            collect_sites(body, pure_fns, out);
        }
        Expression::Function {
            body: Some(body), ..
        } => collect_sites(body, pure_fns, out),
        Expression::Implementation { methods, .. } => {
            for m in methods {
                collect_sites(m, pure_fns, out);
            }
        }
        Expression::Method(_, inner) | Expression::Member(inner) => {
            collect_sites(inner, pure_fns, out);
        }
        _ => {}
    }
}

fn detect_fork_site(
    name: &str,
    args: &Output<'_>,
    body: &Output<'_>,
    pure_fns: &HashSet<String>,
) -> Option<ParForkSite> {
    let (param_names, param_int_like) = fn_params(args)?;
    let ctx = FnCtx {
        fn_name: name,
        param_names,
        param_int_like,
        pure_fns,
    };
    let mut scan = Scan {
        ctx: &ctx,
        guards: Vec::new(),
        found: Vec::new(),
    };
    scan.walk(body, false);
    let found = scan.found;
    let clear = |s: &ParForkSite| !s.guards.iter().any(|g| matches!(g, ParGuard::Opaque));
    // Prefer return-path sites whose path conditions are fully evaluable so
    // AlwaysPar clones stay sound; fall back to any clear site, then any site
    // (opaque ones are kept for detection tests but never specialize).
    let best = found
        .iter()
        .find(|(on_return, s)| *on_return && clear(s))
        .or_else(|| found.iter().find(|(_, s)| clear(s)))
        .or_else(|| found.iter().find(|(on_return, _)| *on_return))
        .or_else(|| found.first())?;
    Some(best.1.clone())
}

/// `(names, int_like)` for the declared parameters, in order.
fn fn_params(args: &Output<'_>) -> Option<(Vec<String>, Vec<bool>)> {
    let items = match args.1.as_ref() {
        Expression::Fragment(items) | Expression::Block(items) => items.as_slice(),
        _ => return None,
    };
    let mut names = Vec::new();
    let mut int_like = Vec::new();
    for item in items {
        let Expression::Argument { name, ty, .. } = peel(item).1.as_ref() else {
            continue;
        };
        let ty_name = ty.as_ref().and_then(|t| match peel(t).1.as_ref() {
            Expression::Type(n) | Expression::Identifier(n) => Some(*n),
            _ => None,
        });
        names.push((*name).to_string());
        int_like.push(matches!(ty_name, Some("int") | Some("byte")));
    }
    Some((names, int_like))
}

/// Depth-first body walk recording every fork site, tagged with whether it sits
/// on a return path (those win when picking the function's primary site) and
/// with the path condition that must hold to reach it.
struct Scan<'a> {
    ctx: &'a FnCtx<'a>,
    guards: Vec<ParGuard>,
    found: Vec<(bool, ParForkSite)>,
}

impl Scan<'_> {
    /// Walk `body` under one extra guard, then restore the guard stack.
    fn walk_guarded(&mut self, body: &Output<'_>, on_return: bool, guard: ParGuard) {
        let depth = self.guards.len();
        self.guards.push(guard);
        self.walk(body, on_return);
        self.guards.truncate(depth);
    }

    /// Statement list: each item may narrow the path condition for its successors.
    fn walk_block(&mut self, items: &[Output<'_>], on_return: bool) {
        let depth = self.guards.len();
        for item in items {
            self.walk(item, on_return);
            match diverging_if_negation(item, self.ctx) {
                Some(guard) => self.guards.push(guard),
                // Any other statement that might return leaves the path
                // condition unknown for everything after it.
                None if contains_return(item) => self.guards.push(ParGuard::Opaque),
                None => {}
            }
        }
        self.guards.truncate(depth);
    }

    fn walk(&mut self, ast: &Output<'_>, on_return: bool) {
        if let Some(site) = match_fork(ast, self.ctx, &self.guards) {
            self.found.push((on_return, site));
        }
        match ast.1.as_ref() {
            Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
                self.walk_block(items, on_return);
            }
            Expression::List(items) | Expression::Array(items) | Expression::Tuple(items) => {
                for item in items {
                    self.walk(item, on_return);
                }
            }
            // Else-if chain: every earlier condition failed for a later arm.
            Expression::If(items) => {
                let depth = self.guards.len();
                for item in items {
                    self.walk(item, on_return);
                    if let Expression::Branch(Some(cond), _) = peel(item).1.as_ref() {
                        self.guards.push(guard_from_cond(cond, self.ctx, false));
                    }
                }
                self.guards.truncate(depth);
            }
            Expression::Return(inner) | Expression::ImplicitReturn(inner) => {
                self.walk(inner, true);
            }
            Expression::Statement(inner)
            | Expression::Expr(inner)
            | Expression::ExprStatement(inner)
            | Expression::Group(inner)
            | Expression::Negate(inner)
            | Expression::Positive(inner)
            | Expression::Not(inner)
            | Expression::LogicalNot(inner)
            | Expression::Cast(inner, _)
            | Expression::Try(inner)
            | Expression::Readonly(inner) => self.walk(inner, on_return),
            Expression::Add(a, b)
            | Expression::Sub(a, b)
            | Expression::Mul(a, b)
            | Expression::Div(a, b)
            | Expression::Mod(a, b)
            | Expression::Eq(a, b)
            | Expression::Neq(a, b)
            | Expression::Le(a, b)
            | Expression::Gt(a, b)
            | Expression::Leq(a, b)
            | Expression::Geq(a, b)
            | Expression::Coalesce(a, b)
            | Expression::Assignment(a, b) => {
                self.walk(a, on_return);
                self.walk(b, on_return);
            }
            Expression::Call { name, args } => {
                self.walk(name, on_return);
                for a in args.iter().flatten() {
                    self.walk(a, on_return);
                }
            }
            Expression::Construct { fields, .. } => match fields {
                EnumConstructPayload::Tuple(items) => {
                    for item in items {
                        self.walk(item, on_return);
                    }
                }
                EnumConstructPayload::Record(fields) => {
                    for f in fields {
                        self.walk(&f.value, on_return);
                    }
                }
                EnumConstructPayload::Unit => {}
            },
            Expression::Branch(cond, body) => match cond {
                Some(c) => {
                    self.walk(c, on_return);
                    self.walk_guarded(body, on_return, guard_from_cond(c, self.ctx, true));
                }
                None => self.walk(body, on_return),
            },
            // Arms are scanned independently — a fork never spans two arms.
            // Constructor patterns are not evaluable from const int args, so
            // those bodies stay opaque. Irrefutable Binding / Wildcard arms
            // inherit only the outer path guards (fork-inside-arm is OK).
            Expression::Match { scrutinee, arms } => {
                self.walk(scrutinee, on_return);
                for arm in arms {
                    match &arm.pattern.1 {
                        Pattern::Wildcard | Pattern::Binding { .. } => {
                            self.walk(&arm.body, on_return);
                        }
                        Pattern::Constructor { .. } => {
                            self.walk_guarded(&arm.body, on_return, ParGuard::Opaque);
                        }
                    }
                }
            }
        Expression::Loop { iterable, body, .. } => {
                self.walk(iterable, on_return);
                self.walk_guarded(body, on_return, ParGuard::Opaque);
            }
            Expression::Variable(_, Some(init)) | Expression::Constant(_, Some(init)) => {
                self.walk(init, on_return)
            }
            Expression::LetDestructure { rhs, .. } => self.walk(rhs, on_return),
            _ => {}
        }
    }
}

/// `if cond { … return … }` with no `else`: everything after it runs with
/// `cond` false. Returns `None` for statements that are not a diverging `if`.
fn diverging_if_negation(item: &Output<'_>, ctx: &FnCtx<'_>) -> Option<ParGuard> {
    let Expression::If(branches) = peel(item).1.as_ref() else {
        return None;
    };
    let [branch] = branches.as_slice() else {
        return None;
    };
    let Expression::Branch(Some(cond), body) = peel(branch).1.as_ref() else {
        return None;
    };
    block_always_returns(body).then(|| guard_from_cond(cond, ctx, false))
}

/// True when the last statement of `body` is a `return` / `raise`.
fn block_always_returns(body: &Output<'_>) -> bool {
    let body = peel(body);
    let last = match body.1.as_ref() {
        Expression::Block(items) | Expression::Fragment(items) | Expression::Program(items) => {
            match items.last() {
                Some(last) => last,
                None => return false,
            }
        }
        _ => body,
    };
    matches!(
        peel(last).1.as_ref(),
        Expression::Return(_) | Expression::ImplicitReturn(_) | Expression::Raise(_)
    )
}

/// Whether a statement can transfer control out of the enclosing function.
///
/// Covers the same node set as [`Scan::walk`]; anything else is a leaf
/// expression that cannot hold a `return`.
fn contains_return(ast: &Output<'_>) -> bool {
    let any = |items: &[Output<'_>]| items.iter().any(contains_return);
    match ast.1.as_ref() {
        Expression::Return(_) | Expression::ImplicitReturn(_) | Expression::Raise(_) => true,
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::List(items)
        | Expression::Array(items)
        | Expression::Tuple(items)
        | Expression::If(items) => any(items),
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner)
        | Expression::Negate(inner)
        | Expression::Positive(inner)
        | Expression::Not(inner)
        | Expression::LogicalNot(inner)
        | Expression::Cast(inner, _)
        | Expression::Try(inner)
        | Expression::Readonly(inner)
        | Expression::Variable(_, Some(inner))
        | Expression::Constant(_, Some(inner))
        | Expression::LetDestructure { rhs: inner, .. } => contains_return(inner),
        Expression::Add(a, b)
        | Expression::Sub(a, b)
        | Expression::Mul(a, b)
        | Expression::Div(a, b)
        | Expression::Mod(a, b)
        | Expression::Eq(a, b)
        | Expression::Neq(a, b)
        | Expression::Le(a, b)
        | Expression::Gt(a, b)
        | Expression::Leq(a, b)
        | Expression::Geq(a, b)
        | Expression::Coalesce(a, b)
        | Expression::Assignment(a, b) => contains_return(a) || contains_return(b),
        Expression::Call { name, args } => {
            contains_return(name) || args.iter().flatten().any(contains_return)
        }
        Expression::Construct { fields, .. } => match fields {
            EnumConstructPayload::Tuple(items) => any(items),
            EnumConstructPayload::Record(fields) => {
                fields.iter().any(|f| contains_return(&f.value))
            }
            EnumConstructPayload::Unit => false,
        },
        Expression::Branch(cond, body) => {
            cond.as_ref().is_some_and(contains_return) || contains_return(body)
        }
        Expression::Match { scrutinee, arms } => {
            contains_return(scrutinee) || arms.iter().any(|a| contains_return(&a.body))
        }
        Expression::Loop { iterable, body, .. } => {
            contains_return(iterable) || contains_return(body)
        }
        _ => false,
    }
}

/// Recognize `cond` as an integer comparison over parameters; anything else is
/// [`ParGuard::Opaque`] so the fork site is never specialized.
fn guard_from_cond(cond: &Output<'_>, ctx: &FnCtx<'_>, expect: bool) -> ParGuard {
    let cond = peel(cond);
    if let Expression::LogicalNot(inner) | Expression::Not(inner) = cond.1.as_ref() {
        return guard_from_cond(inner, ctx, !expect);
    }
    let (op, a, b) = match cond.1.as_ref() {
        Expression::Le(a, b) => (CmpOp::Lt, a, b),
        Expression::Leq(a, b) => (CmpOp::Leq, a, b),
        Expression::Gt(a, b) => (CmpOp::Gt, a, b),
        Expression::Geq(a, b) => (CmpOp::Geq, a, b),
        Expression::Eq(a, b) => (CmpOp::Eq, a, b),
        Expression::Neq(a, b) => (CmpOp::Neq, a, b),
        _ => return ParGuard::Opaque,
    };
    match (guard_operand(a, ctx), guard_operand(b, ctx)) {
        (Some(lhs), Some(rhs)) => ParGuard::Cmp {
            lhs,
            op,
            rhs,
            expect,
        },
        _ => ParGuard::Opaque,
    }
}

/// Comparison operand: an int literal or an int-like parameter (± a literal).
fn guard_operand(expr: &Output<'_>, ctx: &FnCtx<'_>) -> Option<ArgForm> {
    let form = arg_form(expr, ctx)?;
    match form {
        ArgForm::Const(_) => Some(form),
        ArgForm::Param(i) => ctx
            .param_int_like
            .get(i)
            .copied()
            .unwrap_or(false)
            .then_some(form),
        ArgForm::ParamMinus { .. } => Some(form),
    }
}

/// Recognize the IPA fork shapes at `expr` (no recursion into subtrees).
fn match_fork(expr: &Output<'_>, ctx: &FnCtx<'_>, guards: &[ParGuard]) -> Option<ParForkSite> {
    let expr = peel(expr);
    match expr.1.as_ref() {
        Expression::Add(a, b) => binop_site(ctx, guards, a, b, ParBinOp::Add),
        Expression::Sub(a, b) => binop_site(ctx, guards, a, b, ParBinOp::Sub),
        Expression::Mul(a, b) => binop_site(ctx, guards, a, b, ParBinOp::Mul),
        Expression::Tuple(items) => {
            let arms = pure_call_arms(items, ctx)?;
            site(ctx, guards, arms, ParCombine::Tuple)
        }
        Expression::Construct {
            enum_name,
            variant_name,
            fields: EnumConstructPayload::Tuple(items),
        } => {
            let arms = pure_call_arms(items, ctx)?;
            site(
                ctx,
                guards,
                arms,
                ParCombine::EnumCtor {
                    enum_name: (*enum_name).to_string(),
                    variant_name: (*variant_name).to_string(),
                },
            )
        }
        Expression::Construct {
            enum_name,
            variant_name,
            fields: EnumConstructPayload::Record(fields),
        } => {
            let items: Vec<&Output<'_>> = fields.iter().map(|f| &f.value).collect();
            let arms = pure_call_arm_refs(&items, ctx)?;
            site(
                ctx,
                guards,
                arms,
                ParCombine::EnumCtor {
                    enum_name: (*enum_name).to_string(),
                    variant_name: (*variant_name).to_string(),
                },
            )
        }
        Expression::Call {
            name,
            args: Some(args),
        } => {
            let arms = pure_call_arms(args, ctx)?;
            let callee = callee_name(name)?;
            let combine = if callee == ctx.fn_name {
                ParCombine::SelfCall
            } else if ctx.is_pure_callee(callee) {
                ParCombine::ApplyCall {
                    fn_name: callee.to_string(),
                }
            } else {
                return None;
            };
            site(ctx, guards, arms, combine)
        }
        _ => None,
    }
}

fn binop_site(
    ctx: &FnCtx<'_>,
    guards: &[ParGuard],
    a: &Output<'_>,
    b: &Output<'_>,
    op: ParBinOp,
) -> Option<ParForkSite> {
    let arms = vec![pure_call_arm(a, ctx)?, pure_call_arm(b, ctx)?];
    site(ctx, guards, arms, ParCombine::BinOp(op))
}

fn site(
    ctx: &FnCtx<'_>,
    guards: &[ParGuard],
    arms: Vec<ParArm>,
    combine: ParCombine,
) -> Option<ParForkSite> {
    if arms.len() < 2 {
        return None;
    }
    Some(ParForkSite {
        fn_name: ctx.fn_name.to_string(),
        param_count: ctx.param_names.len(),
        arms,
        combine,
        guards: guards.to_vec(),
    })
}

/// Every operand must be an independent pure call: the combine consumes arm
/// results positionally, so a mixed operand list is not representable.
fn pure_call_arms(items: &[Output<'_>], ctx: &FnCtx<'_>) -> Option<Vec<ParArm>> {
    if items.len() < 2 {
        return None;
    }
    items.iter().map(|i| pure_call_arm(i, ctx)).collect()
}

fn pure_call_arm_refs(items: &[&Output<'_>], ctx: &FnCtx<'_>) -> Option<Vec<ParArm>> {
    if items.len() < 2 {
        return None;
    }
    items.iter().map(|i| pure_call_arm(i, ctx)).collect()
}

fn pure_call_arm(expr: &Output<'_>, ctx: &FnCtx<'_>) -> Option<ParArm> {
    let expr = peel(expr);
    let Expression::Call {
        name,
        args: Some(args),
    } = expr.1.as_ref()
    else {
        return None;
    };
    let callee = callee_name(name)?;
    if !ctx.is_pure_callee(callee) {
        return None;
    }
    // Arms of the enclosing specialization are keyed by the *enclosing*
    // function's params; each arm's forms must still parse under that ctx.
    let forms = args
        .iter()
        .map(|a| arg_form(a, ctx))
        .collect::<Option<Vec<_>>>()?;
    Some(ParArm::Call {
        callee: callee.to_string(),
        args: forms,
    })
}

fn arg_form(expr: &Output<'_>, ctx: &FnCtx<'_>) -> Option<ArgForm> {
    let expr = peel(expr);
    match expr.1.as_ref() {
        Expression::Integer(k) => Some(ArgForm::Const(*k)),
        Expression::Identifier(p) => ctx.param_index(p).map(ArgForm::Param),
        Expression::Sub(lhs, rhs) => {
            let (Expression::Identifier(p), Expression::Integer(k)) =
                (peel(lhs).1.as_ref(), peel(rhs).1.as_ref())
            else {
                return None;
            };
            if *k <= 0 {
                return None;
            }
            let idx = ctx.param_index(p)?;
            ctx.param_int_like
                .get(idx)
                .copied()
                .unwrap_or(false)
                .then_some(ArgForm::ParamMinus {
                    param: idx,
                    sub: *k,
                })
        }
        _ => None,
    }
}

fn callee_name<'a>(name: &'a Output<'a>) -> Option<&'a str> {
    match peel(name).1.as_ref() {
        Expression::Identifier(n) => Some(*n),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Constant call-site collection
// ---------------------------------------------------------------------------

fn collect_const_calls(
    ast: &Output<'_>,
    work: &mut WorkEstimate<'_>,
    out: &mut HashMap<String, BTreeSet<Vec<i64>>>,
) {
    let sites = work.sites;
    match ast.1.as_ref() {
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::List(items)
        | Expression::Array(items)
        | Expression::Tuple(items)
        | Expression::If(items) => {
            for item in items {
                collect_const_calls(item, work, out);
            }
        }
        Expression::Module(_, body)
        | Expression::Statement(body)
        | Expression::Expr(body)
        | Expression::ExprStatement(body)
        | Expression::Group(body)
        | Expression::Return(body)
        | Expression::ImplicitReturn(body)
        | Expression::Negate(body)
        | Expression::Not(body)
        | Expression::LogicalNot(body)
        | Expression::Positive(body)
        | Expression::Cast(body, _)
        | Expression::Try(body)
        | Expression::Readonly(body) => collect_const_calls(body, work, out),
        Expression::Add(a, b)
        | Expression::Sub(a, b)
        | Expression::Mul(a, b)
        | Expression::Div(a, b)
        | Expression::Mod(a, b)
        | Expression::Assignment(a, b)
        | Expression::Eq(a, b)
        | Expression::Neq(a, b)
        | Expression::Le(a, b)
        | Expression::Gt(a, b)
        | Expression::Leq(a, b)
        | Expression::Geq(a, b)
        | Expression::Coalesce(a, b) => {
            collect_const_calls(a, work, out);
            collect_const_calls(b, work, out);
        }
        Expression::Call { name, args } => {
            collect_const_calls(name, work, out);
            let Some(args) = args else {
                return;
            };
            for a in args {
                collect_const_calls(a, work, out);
            }
            let Some(fname) = callee_name(name) else {
                return;
            };
            let Some(site) = sites.get(fname) else {
                return;
            };
            if args.len() != site.param_count {
                return;
            }
            let consts = args
                .iter()
                .map(|a| match peel(a).1.as_ref() {
                    Expression::Integer(n) => Some(*n),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>();
            if let Some(consts) = consts {
                if work.worth_parallel(fname, &consts) {
                    out.entry(fname.to_string()).or_default().insert(consts);
                }
            }
        }
        Expression::Construct { fields, .. } => match fields {
            EnumConstructPayload::Tuple(items) => {
                for item in items {
                    collect_const_calls(item, work, out);
                }
            }
            EnumConstructPayload::Record(fields) => {
                for f in fields {
                    collect_const_calls(&f.value, work, out);
                }
            }
            EnumConstructPayload::Unit => {}
        },
        Expression::Branch(cond, body) => {
            if let Some(c) = cond {
                collect_const_calls(c, work, out);
            }
            collect_const_calls(body, work, out);
        }
        Expression::Match { scrutinee, arms } => {
            collect_const_calls(scrutinee, work, out);
            for arm in arms {
                collect_const_calls(&arm.body, work, out);
            }
        }
        Expression::Loop {
            identifier,
            iterable,
            body,
        } => {
            if let Some(id) = identifier {
                collect_const_calls(id, work, out);
            }
            collect_const_calls(iterable, work, out);
            collect_const_calls(body, work, out);
        }
        Expression::Variable(_, Some(init)) | Expression::Constant(_, Some(init)) => {
            collect_const_calls(init, work, out);
        }
        Expression::LetDestructure { rhs, .. } => collect_const_calls(rhs, work, out),
        Expression::Function {
            body: Some(body), ..
        }
        | Expression::Lambda { body, .. }
        | Expression::Defer { body, .. } => collect_const_calls(body, work, out),
        Expression::Implementation { methods, .. } => {
            for m in methods {
                collect_const_calls(m, work, out);
            }
        }
        Expression::Method(_, inner) | Expression::Member(inner) => {
            collect_const_calls(inner, work, out);
        }
        _ => {}
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

    fn parse(src: &str) -> Output<'static> {
        // Leak for test simplicity — parse owns string via 'static trick.
        let owned = Box::leak(src.to_string().into_boxed_str());
        Pratt::default().parse(owned).expect("parse")
    }

    fn sites_of(src: &str) -> HashMap<String, ParForkSite> {
        let ast = parse(src);
        let pure = analyze_pure_fns(&ast);
        analyze_par_fork_sites(&ast, &pure)
    }

    fn arm_args(site: &ParForkSite, i: usize) -> &[ArgForm] {
        let ParArm::Call { args, .. } = &site.arms[i];
        args
    }

    fn arm_callee_at(site: &ParForkSite, i: usize) -> &str {
        arm_callee(&site.arms[i])
    }

    #[test]
    fn detects_fib_shape_and_const_calls() {
        let ast = parse(
            r#"
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let x = fib(32);
    return;
}
"#,
        );
        let pure = analyze_pure_fns(&ast);
        assert!(pure.contains("fib"));
        let sites = analyze_par_fork_sites(&ast, &pure);
        let fib = sites.get("fib").expect("fib fork site");
        assert_eq!(fib.param_count, 1);
        assert_eq!(fib.combine, ParCombine::BinOp(ParBinOp::Add));
        assert_eq!(arm_args(fib, 0), [ArgForm::ParamMinus { param: 0, sub: 1 }]);
        assert_eq!(arm_args(fib, 1), [ArgForm::ParamMinus { param: 0, sub: 2 }]);

        let demanded = collect_par_specialization_args(&ast, &sites);
        let set = demanded.get("fib").expect("fib demands");
        assert!(set.contains(&vec![32]));
        // This fib bottoms out at `n <= 2`, one level earlier than the shape
        // the threshold is calibrated on, so its chain stops one level higher.
        assert!(set.contains(&vec![22])); // chain toward threshold
        assert!(!set.contains(&vec![21]));
    }

    #[test]
    fn detects_mul_binop_fork() {
        let sites = sites_of(
            r#"
fn tree(int n) -> int {
    if n <= 1 { return 1; }
    return tree(n - 1) * tree(n - 2);
}
fn main() { return; }
"#,
        );
        let tree = sites.get("tree").expect("tree fork site");
        assert_eq!(tree.combine, ParCombine::BinOp(ParBinOp::Mul));
        assert_eq!(
            arm_args(tree, 0),
            [ArgForm::ParamMinus { param: 0, sub: 1 }]
        );
        assert_eq!(
            arm_args(tree, 1),
            [ArgForm::ParamMinus { param: 0, sub: 2 }]
        );
    }

    #[test]
    fn detects_sub_binop_fork() {
        let sites = sites_of(
            r#"
fn diff(int n) -> int {
    if n <= 1 { return n; }
    return diff(n - 1) - diff(n - 2);
}
fn main() { return; }
"#,
        );
        let diff = sites.get("diff").expect("diff fork site");
        assert_eq!(diff.combine, ParCombine::BinOp(ParBinOp::Sub));
        assert_eq!(
            arm_args(diff, 0),
            [ArgForm::ParamMinus { param: 0, sub: 1 }]
        );
        assert_eq!(
            arm_args(diff, 1),
            [ArgForm::ParamMinus { param: 0, sub: 2 }]
        );
    }

    #[test]
    fn rejects_single_recursive_arm() {
        let sites = sites_of(
            r#"
fn fib(int n) -> int {
    if n <= 1 { return n; }
    return fib(n - 1) + (n - 2);
}
fn main() { return; }
"#,
        );
        assert!(
            !sites.contains_key("fib"),
            "single recursive arm must not be a fork site: {sites:?}"
        );
    }

    #[test]
    fn detects_enum_ctor_fork() {
        let sites = sites_of(
            r#"
enum Tree {
    Leaf,
    Node(Tree, Tree),
}
fn build(int n) -> Tree {
    if n <= 1 { return Tree::Leaf; }
    return Tree::Node(build(n - 1), build(n - 2));
}
fn main() { return; }
"#,
        );
        let build = sites.get("build").expect("build fork site");
        assert_eq!(
            build.combine,
            ParCombine::EnumCtor {
                enum_name: "Tree".to_string(),
                variant_name: "Node".to_string(),
            }
        );
        assert_eq!(build.arms.len(), 2);
        assert_eq!(
            arm_args(build, 0),
            [ArgForm::ParamMinus { param: 0, sub: 1 }]
        );
    }

    #[test]
    fn detects_tak_self_call_combine() {
        let sites = sites_of(
            r#"
fn tak(int x, int y, int z) -> int {
    if y < x {
        return tak(tak(x - 1, y, z), tak(y - 1, z, x), tak(z - 1, x, y));
    }
    return z;
}
fn main() { return; }
"#,
        );
        let tak = sites.get("tak").expect("tak fork site");
        assert_eq!(tak.combine, ParCombine::SelfCall);
        assert_eq!(tak.param_count, 3);
        assert_eq!(tak.arms.len(), 3);
        assert_eq!(
            arm_args(tak, 0),
            [
                ArgForm::ParamMinus { param: 0, sub: 1 },
                ArgForm::Param(1),
                ArgForm::Param(2)
            ]
        );
        assert_eq!(
            arm_args(tak, 2),
            [
                ArgForm::ParamMinus { param: 2, sub: 1 },
                ArgForm::Param(0),
                ArgForm::Param(1)
            ]
        );
        assert_eq!(
            eval_arm_args(&tak.arms[1], &[18, 12, 6]),
            Some(vec![11, 6, 18])
        );
    }

    #[test]
    fn detects_fork_inside_match_arm() {
        let sites = sites_of(
            r#"
enum Mode {
    Fast,
    Slow,
}
fn pick(int n) -> Mode {
    if n <= 1 { return Mode::Fast; }
    return Mode::Slow;
}
fn fibm(int n) -> int {
    return match pick(n) {
        Mode::Fast => 1,
        Mode::Slow => fibm(n - 1) + fibm(n - 2),
    };
}
fn main() { return; }
"#,
        );
        let fibm = sites.get("fibm").expect("fibm fork site");
        assert_eq!(fibm.combine, ParCombine::BinOp(ParBinOp::Add));
        assert_eq!(
            arm_args(fibm, 0),
            [ArgForm::ParamMinus { param: 0, sub: 1 }]
        );
        assert_eq!(
            arm_args(fibm, 1),
            [ArgForm::ParamMinus { param: 0, sub: 2 }]
        );
    }

    #[test]
    fn below_threshold_and_dynamic_args_do_not_demand_specs() {
        let t = par_cost_threshold();
        let ast = parse(&format!(
            r#"
fn fib(int n) -> int {{
    if n <= 1 {{ return n; }}
    return fib(n - 1) + fib(n - 2);
}}
fn main() {{
    let k = {t};
    let a = fib({t});
    let b = fib(k);
    return;
}}
"#
        ));
        let pure = analyze_pure_fns(&ast);
        let sites = analyze_par_fork_sites(&ast, &pure);
        let demanded = collect_par_specialization_args(&ast, &sites);
        assert!(
            demanded.get("fib").is_none(),
            "arg == threshold and dynamic args must not demand specs: {demanded:?}"
        );
        assert!(!args_worth_parallel(&sites, "fib", &[t]));
        assert!(args_worth_parallel(&sites, "fib", &[t + 1]));
        assert!(!args_worth_parallel(&sites, "fib", &[]));
        assert!(!args_worth_parallel(&sites, "nosuch", &[t + 1]));
    }

    /// The score is expressed in fib-equivalent units, so the canonical shape
    /// scores its own argument and the threshold keeps its old meaning there.
    #[test]
    fn fib_shape_scores_its_own_argument() {
        let t = par_cost_threshold();
        let sites = sites_of(
            r#"
fn fib(int n) -> int {
    if n <= 1 { return n; }
    return fib(n - 1) + fib(n - 2);
}
fn main() { return; }
"#,
        );
        for n in 2..=t {
            assert_eq!(
                par_work_units(&sites, "fib", &[n]),
                n,
                "fib({n}) must score {n} units"
            );
        }
        // Above the cutoff the count saturates — the verdict cannot change.
        assert_eq!(par_work_units(&sites, "fib", &[t + 1]), t + 1);
        assert_eq!(par_work_units(&sites, "fib", &[32]), t + 1);
        assert!(args_worth_parallel(&sites, "fib", &[32]));
    }

    /// Arms into a callee with no fork site (or none at all) are leaves, so a
    /// site whose work the analysis cannot see stays sequential.
    #[test]
    fn trivial_helper_arms_score_below_threshold() {
        let sites = sites_of(
            r#"
fn sq(int n) -> int {
    return n * n;
}
fn pair_sq(int n) -> int {
    if n <= 0 { return 0; }
    return sq(n) + sq(n - 1);
}
fn main() { return; }
"#,
        );
        assert!(sites.contains_key("pair_sq"), "fork site still detected");
        assert_eq!(par_work_units(&sites, "pair_sq", &[22]), 2);
        assert!(!args_worth_parallel(&sites, "pair_sq", &[22]));
    }

    /// Heavy helper arms carry their own subtree, so the parent forks even
    /// though it is not itself recursive.
    #[test]
    fn recursive_helper_arms_score_above_threshold() {
        let sites = sites_of(
            r#"
fn fib(int n) -> int {
    if n <= 1 { return n; }
    return fib(n - 1) + fib(n - 2);
}
fn pair_fib(int n) -> int {
    if n <= 0 { return 0; }
    return fib(n) + fib(n - 1);
}
fn main() { return; }
"#,
        );
        assert!(args_worth_parallel(&sites, "pair_fib", &[24]));
        assert!(!args_worth_parallel(&sites, "pair_fib", &[10]));
    }

    /// Rotating `tak` arms keep a large component alive, but most children miss
    /// the `y < x` guard and the `SelfCall` combine's re-entry is unknowable,
    /// so the fair benchmark load scores just *under* the cutoff and refuses.
    /// Only a load with a genuinely deeper tree crosses it.
    #[test]
    fn fair_tak_load_scores_below_threshold() {
        let t = par_cost_threshold();
        let sites = sites_of(
            r#"
fn tak(int x, int y, int z) -> int {
    if y >= x {
        return z;
    }
    return tak(tak(x - 1, y, z), tak(y - 1, z, x), tak(z - 1, x, y));
}
fn main() { return; }
"#,
        );
        assert_eq!(par_work_units(&sites, "tak", &[18, 12, 6]), t);
        assert!(
            !args_worth_parallel(&sites, "tak", &[18, 12, 6]),
            "the fair tak(18, 12, 6) load must stay sequential"
        );
        // `max(args)` alone rated this above the threshold; it is 53 calls.
        assert!(
            !args_worth_parallel(&sites, "tak", &[24, 22, 20]),
            "a narrow x - y gap is cheap however large the args"
        );
        assert!(args_worth_parallel(&sites, "tak", &[21, 12, 6]));
        assert!(args_worth_parallel(&sites, "tak", &[24, 16, 8]));
    }

    /// A cyclic arg graph (`Const` forms can raise a component) must not hang
    /// the estimator; re-entrant vectors are leaves.
    #[test]
    fn cyclic_arg_forms_terminate_the_estimator() {
        let sites = sites_of(
            r#"
fn ping(int n, int m) -> int {
    if n <= 0 { return m; }
    return ping(n - 1, 9) + ping(n - 1, m);
}
fn main() { return; }
"#,
        );
        assert!(sites.contains_key("ping"), "fork site detected");
        let _ = par_work_units(&sites, "ping", &[40, 3]);
    }

    /// `f(z - 1, x, y)` keeps a large param alive in every child, so the
    /// closure only terminates because negative args and the budget cut it.
    #[test]
    fn tak_specialization_closure_is_bounded() {
        let ast = parse(
            r#"
fn tak(int x, int y, int z) -> int {
    if y < x {
        return tak(tak(x - 1, y, z), tak(y - 1, z, x), tak(z - 1, x, y));
    }
    return z;
}
fn main() {
    let a = tak(21, 12, 6);
    return;
}
"#,
        );
        let pure = analyze_pure_fns(&ast);
        let sites = analyze_par_fork_sites(&ast, &pure);
        let demanded = collect_par_specialization_args(&ast, &sites);
        let set = demanded.get("tak").expect("tak demands");
        assert!(
            set.contains(&vec![21, 12, 6]),
            "root call site must survive"
        );
        assert!(
            set.len() <= PAR_SPEC_BUDGET,
            "budget exceeded: {}",
            set.len()
        );
        assert!(
            set.iter().all(|args| args.iter().all(|a| *a >= 0)),
            "negative arg vectors must not be demanded: {set:?}"
        );
    }

    /// A clone has no base case, so arg vectors that take the early return
    /// must never be specialized (`tak(0, 0, 21)` returns `z` immediately).
    #[test]
    fn base_case_arg_vectors_are_not_specialized() {
        let ast = parse(
            r#"
fn tak(int x, int y, int z) -> int {
    if y >= x {
        return z;
    }
    return tak(tak(x - 1, y, z), tak(y - 1, z, x), tak(z - 1, x, y));
}
fn main() {
    let a = tak(21, 12, 6);
    let b = tak(0, 0, 21);
    return;
}
"#,
        );
        let pure = analyze_pure_fns(&ast);
        let sites = analyze_par_fork_sites(&ast, &pure);
        let tak = sites.get("tak").expect("tak fork site");
        assert_eq!(
            tak.guards,
            vec![ParGuard::Cmp {
                lhs: ArgForm::Param(1),
                op: CmpOp::Geq,
                rhs: ArgForm::Param(0),
                expect: false,
            }]
        );
        let demanded = collect_par_specialization_args(&ast, &sites);
        let set = demanded.get("tak").expect("tak demands");
        assert!(set.contains(&vec![21, 12, 6]));
        assert!(
            !set.contains(&vec![0, 0, 21]),
            "base-case vector must not be specialized: {set:?}"
        );
    }

    /// An early return the analysis cannot evaluate must disable the site.
    #[test]
    fn opaque_guard_blocks_specialization() {
        let ast = parse(
            r#"
fn f(int n) -> int {
    if n % 2 == 0 { return 1; }
    return f(n - 1) + f(n - 2);
}
fn main() {
    let a = f(32);
    return;
}
"#,
        );
        let pure = analyze_pure_fns(&ast);
        let sites = analyze_par_fork_sites(&ast, &pure);
        assert_eq!(
            sites.get("f").map(|s| s.guards.as_slice()),
            Some(&[ParGuard::Opaque][..])
        );
        let demanded = collect_par_specialization_args(&ast, &sites);
        assert!(
            demanded.get("f").is_none(),
            "unevaluable guard must block specialization: {demanded:?}"
        );
    }

    #[test]
    fn detects_independent_pure_helper_arms() {
        let sites = sites_of(
            r#"
fn sq(int n) -> int {
    return n * n;
}
fn pair_sq(int n) -> int {
    if n <= 0 { return 0; }
    return sq(n) + sq(n - 1);
}
fn main() { return; }
"#,
        );
        let site = sites.get("pair_sq").expect("helper-arm fork site");
        assert_eq!(site.combine, ParCombine::BinOp(ParBinOp::Add));
        assert_eq!(arm_callee_at(site, 0), "sq");
        assert_eq!(arm_callee_at(site, 1), "sq");
        assert_eq!(arm_args(site, 0), [ArgForm::Param(0)]);
        assert_eq!(
            arm_args(site, 1),
            [ArgForm::ParamMinus { param: 0, sub: 1 }]
        );
    }

    #[test]
    fn detects_apply_call_and_tuple_combines() {
        let sites = sites_of(
            r#"
fn id(int n) -> int { return n; }
fn add2(int a, int b) -> int { return a + b; }
fn pack(int n) -> (int, int) {
    if n <= 0 { return (0, 0); }
    return (id(n), id(n - 1));
}
fn join(int n) -> int {
    if n <= 0 { return 0; }
    return add2(id(n), id(n - 1));
}
fn main() { return; }
"#,
        );
        let pack = sites.get("pack").expect("tuple fork");
        assert_eq!(pack.combine, ParCombine::Tuple);
        let join = sites.get("join").expect("apply-call fork");
        assert_eq!(
            join.combine,
            ParCombine::ApplyCall {
                fn_name: "add2".to_string()
            }
        );
    }

    /// Irrefutable match arms inherit outer path guards so fork-inside-arm emits.
    #[test]
    fn irrefutable_match_arm_fork_keeps_clear_guards() {
        let sites = sites_of(
            r#"
fn fibm(int n) -> int {
    if n <= 1 { return n; }
    return match n {
        _ => fibm(n - 1) + fibm(n - 2),
    };
}
fn main() { return; }
"#,
        );
        let fibm = sites.get("fibm").expect("fibm fork site");
        assert_eq!(fibm.combine, ParCombine::BinOp(ParBinOp::Add));
        assert!(
            !fibm.guards.iter().any(|g| matches!(g, ParGuard::Opaque)),
            "irrefutable match arm must not opaque the fork: {:?}",
            fibm.guards
        );
    }

    #[test]
    fn specialization_names_cover_multi_arg() {
        assert_eq!(par_specialization_name("fib", &[22]), "__coil_par_fib_22");
        assert_eq!(
            par_specialization_name("tak", &[18, 12, 6]),
            "__coil_par_tak_18_12_6"
        );
    }
}
