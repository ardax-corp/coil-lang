//! Static shape + profitability analysis for Independent Parallel Arms (IPA).
//!
//! A *fork site* is an expression whose operands are two or more mutually
//! independent **pure** calls — self-recursion is common but not required:
//! `f(a) ⊕ f(b)`, n-ary `f(a)+g(b)+h(c)` for associative `+`/`*`/`^`,
//! let-bound `let a = f(…); let b = g(…); return a ⊕ b`, `h(a) + h(b)`,
//! `E::V(f(a), f(b))`, `(f(a), f(b))`, the tak-style `f(f(a), f(b), f(c))`,
//! or `g(f(a), f(b))`. Arms are described structurally ([`ArgForm`]) rather
//! than by function allowlists. Constant call sites whose fork-tree grain
//! ([`par_work_grain`]) exceeds [`par_expr_grain_for`] rewrite to one parameterized
//! fork worker ([`par_worker_name`]) that takes the live args. Grain is the
//! const-site entry gate (no hot-path skip-threshold). Nested AlwaysPar is a
//! hop/depth counter on that same worker — not a constellation of frozen arg
//! clones (COI-366 F1). F2 broadens admission under those same gates (COI-368).
//!
//! Detection runs on a module's HIR as built, before any pass
//! ([`HirModule::par_fork_sites`], [`HirModule::par_worker_fns`]). A site
//! is keyed by its function's bare name, which is how its arms name their
//! callees and how the plan finds a call's worker. The walk visits what the
//! plan can turn into a worker: statement lists, `if` chains, `match` arms,
//! the operands of arithmetic and comparisons, call and constructor
//! arguments.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::super::{BodyKind, Builtin, Callee, HirBody, HirFlags, HirId, HirKind, HirModule, HirPat, Lit, LocalId, LocalKind, MakeKind, UnOp, query};
use super::loop_par::{source_op, unwrap_byte};
use crate::typechecking::infer::Checker;
use crate::typechecking::ty::{self as coil_ty, Ty};

/// Expression IPA grain floor.
///
/// Unit is fork-tree nodes `W`, not fib(n). Default [`DEFAULT_EXPR_GRAIN`]
/// is `W(fib(20))` for the `n <= 1` recurrence (`Fib(21) - 1`): the spawn
/// profitability floor previously written as fib-unit 20. Tight sites
/// (unary const-guard, counted `W`) fork iff `W` is strictly greater.
/// Loose sites ([`grain_w_is_tight`] is false) use [`DEFAULT_LOOSE_EXPR_GRAIN`].
pub const DEFAULT_EXPR_GRAIN: i64 = 10_945;

/// Floor for sites whose `W` undercounts sequential work.
///
/// `SelfCall` combine re-entry and multi-parameter guards (tak `y >= x`)
/// leave most recursive calls as sequential leaves that `W` does not count.
/// Default **8000** admits `tak(18, 12, 6)`
/// (`W = 8398`) and keeps tight fib-shaped trees on [`DEFAULT_EXPR_GRAIN`].
pub const DEFAULT_LOOSE_EXPR_GRAIN: i64 = 8_000;

/// Loop IPA grain floor. Unit is trip count of a
/// counted `[begin, end)` range, not expression `W`. Isolate spawn still
/// needs tens of trips, not thousands of fib-tree nodes.
pub const DEFAULT_LOOP_GRAIN: i64 = 20;

/// Per-site expression grain floor, same units as `W`.
///
/// Tight recurrences keep [`DEFAULT_EXPR_GRAIN`]. Sites where `W` is a known
/// undercount use the scaled loose floor so spawn can still pay off.
pub fn par_expr_grain_for(site: &ParForkSite) -> i64 {
    let base = DEFAULT_EXPR_GRAIN;
    if grain_w_is_tight(site) {
        base
    } else {
        loose_expr_grain(base)
    }
}

fn loose_expr_grain(base: i64) -> i64 {
    base.saturating_mul(DEFAULT_LOOSE_EXPR_GRAIN) / DEFAULT_EXPR_GRAIN.max(1)
}

/// True when `W` tracks sequential work at this site.
///
/// A guard that relates two parameters, or a [`ParCombine::SelfCall`] whose
/// re-entry on joined values is invisible to `W`, leaves most recursive
/// calls as sequential leaves. Those sites use the loose floor.
pub fn grain_w_is_tight(site: &ParForkSite) -> bool {
    if matches!(site.combine, ParCombine::SelfCall) {
        return false;
    }
    if site.guards.iter().any(|g| matches!(g, ParGuard::Opaque)) {
        return false;
    }
    !site.guards.iter().any(guard_compares_distinct_params)
}

fn guard_compares_distinct_params(g: &ParGuard) -> bool {
    let ParGuard::Cmp { lhs, rhs, .. } = g else {
        return false;
    };
    match (arg_form_param(lhs), arg_form_param(rhs)) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    }
}

fn arg_form_param(form: &ArgForm) -> Option<usize> {
    match form {
        ArgForm::Param(i) => Some(*i),
        ArgForm::ParamMinus { param, .. } | ArgForm::ParamPlus { param, .. } => Some(*param),
        ArgForm::Const(_) => None,
    }
}

/// Binary op used at a [`ParCombine::BinOp`] fork site.
///
/// `Add` / `Mul` / `Xor` are associative (and commutative) on `int`, so a
/// nested tree of the same op flattens to N arms. `Sub` is not: only a
/// two-arm site is admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParBinOp {
    Add,
    Sub,
    Mul,
    Xor,
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
    /// `param + add` with `add > 0`; requires an int-like parameter.
    ParamPlus { param: usize, add: i64 },
}

/// One independent arm of a fork site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParArm {
    /// Call to a pure function (often self); one [`ArgForm`] per callee
    /// parameter. `span` is the call in the source: where a worker that
    /// makes this call on its own thread is located in a backtrace.
    Call { callee: String, args: Vec<ArgForm>, span: super::super::Span },
}

/// How arm results are recombined once every arm has been joined.
///
/// The combine always consumes the arm results positionally and in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParCombine {
    /// `arm0 ⊕ arm1 [⊕ …]` — two arms for any op; three or more only when
    /// [`ParBinOp`] is associative (`Add` / `Mul` / `Xor`).
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
    /// The function's own name, with its module path or owner
    /// (`f`, `m::f`, `Owner::m`): what a call's key is matched against, as
    /// `fn_name` is shared by a method or another module's function.
    pub qualified: String,
    /// Declared parameter count of the enclosing function (worker arity, plus hop).
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

// Structural grain (fork-tree nodes W)

/// Recursion depth at which the estimator stops descending and calls it a leaf.
const WORK_MAX_DEPTH: u32 = 256;

/// Distinct arg vectors the estimator will memoize before giving up.
const WORK_MEMO_CAP: usize = 1 << 14;

/// Bounded structural estimate of the grain below a fork site.
///
/// Counts the fork-site nodes a concrete arg vector reaches through the arms'
/// [`ArgForm`] transforms, pruning children that miss the site's guards — those
/// are base cases and do no forkable work. Every imprecision (opaque callees, a
/// `SelfCall` combine's re-entry on joined values, the caps below) resolves
/// *downwards*, so the count is a lower bound: unknown structure only refuses.
/// Compared **directly** to [`par_expr_grain_for`] — no fib-unit inversion.
struct WorkEstimate<'a> {
    sites: &'a HashMap<String, ParForkSite>,
    /// Counting past the floor cannot change the verdict, so totals stop here.
    cap: i64,
    memo: HashMap<(&'a str, Vec<i64>), i64>,
}

impl<'a> WorkEstimate<'a> {
    fn new(sites: &'a HashMap<String, ParForkSite>) -> Self {
        Self {
            sites,
            cap: DEFAULT_EXPR_GRAIN.saturating_add(1),
            memo: HashMap::new(),
        }
    }

    /// Fork-tree grain below `fn_name(args)`, saturating one node past the
    /// floor (scores at or below it are exact).
    #[cfg(test)]
    fn grain(&mut self, fn_name: &str, args: &[i64]) -> i64 {
        self.nodes(fn_name, args, 0)
    }

    fn site_floor(&self, fn_name: &str) -> i64 {
        self.sites
            .get(fn_name)
            .map(par_expr_grain_for)
            .unwrap_or(DEFAULT_EXPR_GRAIN)
    }

    fn worth_parallel(&mut self, fn_name: &str, args: &[i64]) -> bool {
        self.nodes(fn_name, args, 0) > self.site_floor(fn_name)
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

/// Fork-tree grain below `fn_name(args)`'s fork site (nodes `W`).
/// Saturates at [`par_expr_grain`] `+ 1`.
#[cfg(test)]
pub fn par_work_grain(sites: &HashMap<String, ParForkSite>, fn_name: &str, args: &[i64]) -> i64 {
    WorkEstimate::new(sites).grain(fn_name, args)
}

/// True when `fn_name(args)` carries more grain than [`par_expr_grain_for`].
pub fn args_worth_parallel(
    sites: &HashMap<String, ParForkSite>,
    fn_name: &str,
    args: &[i64],
) -> bool {
    WorkEstimate::new(sites).worth_parallel(fn_name, args)
}

/// Smallest `n` in `0..=48` at which unary `fn_name(n)` clears the grain floor.
///
/// Only single-parameter fork sites. The runtime check is `arg >= cutoff`;
/// multi-arg sites stay on the constant-call rewrite.
pub fn unary_dynamic_cutoff(sites: &HashMap<String, ParForkSite>, fn_name: &str) -> Option<i64> {
    let site = sites.get(fn_name)?;
    if site.param_count != 1 {
        return None;
    }
    if site.guards.iter().any(|g| matches!(g, ParGuard::Opaque)) {
        return None;
    }
    const HI_CAP: i64 = 48;
    if !args_worth_parallel(sites, fn_name, &[HI_CAP]) {
        return None;
    }
    let mut lo = 0i64;
    let mut hi = HI_CAP;
    while lo + 1 < hi {
        let mid = lo + (hi - lo) / 2;
        if args_worth_parallel(sites, fn_name, &[mid]) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Some(hi)
}

/// Parameterized fork-worker name for `fn_name`.
///
/// `fib` → `__coil_par_fib`; one worker per site, not per const-arg vector.
pub fn par_worker_name(fn_name: &str) -> String {
    format!("{}{fn_name}", common::PAR_FORK_PREFIX)
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
        ArgForm::ParamPlus { param, add } => {
            parent_args.get(*param).and_then(|v| v.checked_add(*add))
        }
    }
}

/// Arm-transform hops the parameterized worker may re-enter itself.
///
/// Nested AlwaysPar down to the work cutoff used to emit `__coil_par_fib_21…n`.
/// A small hop count is a counted evidence gate (COI-361 E3 / COI-366 F1):
/// the const call is the evidence, and the same worker decrements hop rather
/// than cloning a frozen arg vector. Hop 0 / missed path guards fall through
/// to the sequential original — not a grain skip-threshold on the hot path.
pub const PAR_SPEC_HOPS: u32 = 2;

impl HirModule {
    /// The primary fork site of every pure function that has one, by the
    /// function's bare name.
    ///
    /// One site per function: the first clear-path site found walking the
    /// body, preferring return-path sites without opaque guards.
    pub fn par_fork_sites(&self, checker: &Checker, pure_fns: &HashSet<String>) -> HashMap<String, ParForkSite> {
        let mut out = HashMap::new();
        for body in self.fork_bodies() {
            let name = bare(&body.name);
            // The set lists a body under its own name (`f`, `Owner::m`,
            // with the module's path), never a method under its bare one.
            if pure_fns.contains(body.name.as_str())
                && let Some(mut site) = detect_fork_site(name, body, checker, pure_fns)
            {
                site.qualified = body.name.clone();
                out.insert(name.to_string(), site);
            }
        }
        out
    }

    /// Functions that need one parameterized fork worker.
    ///
    /// A name is demanded when some const call reaches its fork site
    /// (guards hold) with grain above its floor. Hops are depth on that
    /// worker, not extra demanded vectors.
    pub fn par_worker_fns(&self, checker: &Checker, sites: &HashMap<String, ParForkSite>) -> HashSet<String> {
        let mut demanded: HashMap<String, BTreeSet<Vec<i64>>> = HashMap::new();
        let mut work = WorkEstimate::new(sites);
        for body in self.fork_bodies() {
            let Some(root) = body.root else { continue };
            let calls = ConstCalls { module: self, checker, sites, enclosing_fork: sites.contains_key(bare(&body.name)) };
            calls.collect(body, root, &mut work, &mut demanded);
        }
        let mut out = HashSet::new();
        for (name, set) in demanded {
            let Some(site) = sites.get(&name) else { continue };
            if site.guards.iter().any(|g| matches!(g, ParGuard::Opaque)) {
                continue;
            }
            if set.iter().any(|args| guards_hold(&site.guards, args) && work.worth_parallel(&name, args)) {
                out.insert(site.qualified.clone());
            }
        }
        out
    }

    /// Functions and inherent methods, in source order (a later function
    /// of the same bare name replaces an earlier one's site). Tests are
    /// left out, and so are trait methods, with the functions declared in
    /// them: a site is keyed by its function's bare name and its worker is
    /// reached from a bare call (`f(32)`), which is never how a trait's
    /// method is called, so a site there could get no worker and would
    /// take the key of a function of the same name.
    pub(super) fn fork_bodies(&self) -> Vec<&HirBody> {
        let mut bodies: Vec<&HirBody> = self
            .bodies
            .iter()
            .filter(|b| matches!(b.kind, BodyKind::Function | BodyKind::Method))
            .filter(|b| !self.in_trait(b) && !b.name.split("::").any(|segment| segment.starts_with("test \"")))
            .collect();
        bodies.sort_by_key(|b| b.span.0);
        bodies
    }
}

impl HirModule {
    /// Whether `body` is a trait's method, an instance's or a default, or
    /// a function declared in one.
    fn in_trait(&self, body: &HirBody) -> bool {
        let path: Vec<&str> = body.name.split("::").collect();
        path.iter().any(|segment| segment.contains(" for "))
            || path.windows(2).any(|pair| self.trait_effects.iter().any(|t| t.trait_name == pair[0] && t.method == pair[1]))
    }
}

pub(super) fn bare(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

/// The function a call names bare (`f(a)`): a path names a static method
/// or another module's function, and `Trait::m(a)` lowers under the bare
/// method name without being a call of a function of that name.
pub(super) fn bare_callee<'a>(checker: &Checker, body: &'a HirBody, id: HirId) -> Option<&'a str> {
    let e = body.expr(id);
    let HirKind::Call { callee: Callee::Named { name, .. }, .. } = &e.kind else { return None };
    (!name.contains("::") && checker.qualified_trait_call_at(e.span.0, e.span.1).is_none()).then_some(name.as_str())
}

/// The function being scanned.
struct FnCtx<'a> {
    checker: &'a Checker,
    body: &'a HirBody,
    fn_name: &'a str,
    /// The declared parameters: an inherent method's `self` is not one.
    params: Vec<LocalId>,
    /// `param - k` forms are only meaningful for int-like parameters.
    param_int_like: Vec<bool>,
    /// Pure callees allowed as fork arms (includes the enclosing fn when pure).
    pure_fns: &'a HashSet<String>,
}

impl FnCtx<'_> {
    fn param_index(&self, local: LocalId) -> Option<usize> {
        self.params.iter().position(|p| *p == local)
    }

    fn int_like(&self, index: usize) -> bool {
        self.param_int_like.get(index).copied().unwrap_or(false)
    }

    fn kind(&self, id: HirId) -> &HirKind {
        &self.body.expr(id).kind
    }

    /// `id` under what the builder wraps a written expression in: the
    /// `& 255` of `byte` arithmetic, and the `ensures` checks on a value.
    fn peel(&self, id: HirId) -> HirId {
        let id = unwrap_byte(self.body, id);
        let e = self.body.expr(id);
        if e.node.is_none()
            && let HirKind::Block { stmts, tail: Some(tail) } = &e.kind
            && let Some(&first) = stmts.first()
            && let HirKind::Let { local, init: Some(init) } = self.body.expr(first).kind
            && self.body.expr(first).node.is_none()
            && self.body.local(local).name == "result"
            && matches!(self.body.expr(*tail).kind, HirKind::Local(l) if l == local)
        {
            return self.peel(init);
        }
        id
    }

    /// A contract check the builder put among a block's statements.
    fn contract_stmt(&self, id: HirId) -> bool {
        in_contract(self.body, id)
    }

    /// The written statements of block `id`.
    fn items(&self, id: HirId) -> Vec<HirId> {
        match self.kind(id) {
            HirKind::Block { stmts, tail } => stmts.iter().chain(tail).copied().filter(|s| !self.contract_stmt(*s)).collect(),
            _ => vec![id],
        }
    }
}

/// Whether `id` is a node the builder made for a contract clause: its
/// check, or the `old(…)` value an `ensures` reads.
pub(super) fn in_contract(body: &HirBody, id: HirId) -> bool {
    let e = body.expr(id);
    e.node.is_none() && body.contract_spans.iter().any(|c| c.0 <= e.span.0 && e.span.1 <= c.1)
}

/// What a `match` the builder made stands for.
pub(super) enum Made {
    /// `e?`.
    Try(HirId),
    /// `a ?? b`.
    Coalesce(HirId, HirId),
    /// `return e` from a `main` that returns a `Result`.
    MainReturn(HirId),
    /// `e?.f`.
    Other,
}

/// `id` as the builder's own `match`, when it wrote no arm of it.
pub(super) fn made_match(body: &HirBody, id: HirId) -> Option<Made> {
    let HirKind::Match { scrutinee, arms } = &body.expr(id).kind else { return None };
    let [first, second] = arms.as_slice() else { return None };
    let hit = body.expr(first.body);
    if hit.node.is_some() {
        return None;
    }
    Some(match hit.kind {
        HirKind::Local(l) if body.local(l).kind == LocalKind::Temp => {
            if second.pat == HirPat::Wild && body.expr(second.body).node.is_some() {
                Made::Coalesce(*scrutinee, second.body)
            } else {
                Made::Try(*scrutinee)
            }
        }
        HirKind::Builtin { op: Builtin::Panic, .. } => Made::MainReturn(*scrutinee),
        _ => Made::Other,
    })
}

/// A `return` or `raise` the source wrote (the builder's own carry no node).
fn written_return(body: &HirBody, id: HirId) -> bool {
    let e = body.expr(id);
    e.node.is_some() && (matches!(e.kind, HirKind::Return(_)) || matches!(made_match(body, id), Some(Made::MainReturn(_))))
}

/// The value of a written `return e`; `None` for `raise e` and `return;`.
pub(super) fn returned(body: &HirBody, id: HirId) -> Option<HirId> {
    if let Some(Made::MainReturn(value)) = made_match(body, id) {
        return Some(value);
    }
    let HirKind::Return(Some(value)) = body.expr(id).kind else { return None };
    let raised = body.expr(value).node.is_none()
        && matches!(&body.expr(value).kind, HirKind::Make { kind: MakeKind::Variant { variant, .. }, .. } if variant == "Err");
    (!raised).then_some(value)
}

/// A `let` of a plain local with its initializer.
fn let_binding(body: &HirBody, id: HirId) -> Option<(LocalId, HirId)> {
    match body.expr(id).kind {
        HirKind::Let { local, init: Some(init) } if body.local(local).kind == LocalKind::Let => Some((local, init)),
        _ => None,
    }
}

/// The operands the fork walk looks into, for a node with no path
/// condition of its own.
fn operands(body: &HirBody, id: HirId) -> Vec<HirId> {
    match &body.expr(id).kind {
        HirKind::Make { kind: MakeKind::Array | MakeKind::Tuple | MakeKind::Variant { .. }, args } => args.clone(),
        HirKind::Un { operand, .. } => vec![*operand],
        HirKind::Cast { value } => vec![*value],
        HirKind::Bin { op, lhs, rhs } if matches!(source_op(*op), "+" | "-" | "*" | "/" | "%" | "^" | "==" | "!=" | "<" | ">" | "<=" | ">=") => {
            vec![*lhs, *rhs]
        }
        HirKind::Assign { place, value } if !body.expr(id).flags.contains(HirFlags::COMPOUND) => vec![*place, *value],
        HirKind::Append { value, .. } => vec![*value],
        HirKind::Call { callee, args } => {
            // A method's receiver and a named argument are not looked into.
            let (head, rest) = match callee {
                Callee::Value(v) => (Some(*v), args.as_slice()),
                Callee::Method { .. } => (None, args.get(1..).unwrap_or(&[])),
                _ => (None, args.as_slice()),
            };
            head.into_iter().chain(rest.iter().copied()).collect()
        }
        HirKind::LetPat { init, .. } => vec![*init],
        _ => Vec::new(),
    }
}

fn detect_fork_site(name: &str, body: &HirBody, checker: &Checker, pure_fns: &HashSet<String>) -> Option<ParForkSite> {
    let root = body.root?;
    // An inherent method's `self` is implicit.
    let own_self = body.kind == BodyKind::Method && body.params.first().is_some_and(|p| body.local(*p).name == "self");
    let params: Vec<LocalId> = body.params.iter().copied().skip(usize::from(own_self)).collect();
    let param_int_like = params
        .iter()
        .map(|p| matches!(&body.local(*p).ty, Some(Ty::Con(n)) if n == coil_ty::INT || n == coil_ty::BYTE))
        .collect();
    let ctx = FnCtx { checker, body, fn_name: name, params, param_int_like, pure_fns };
    let mut scan = Scan { ctx: &ctx, guards: Vec::new(), found: Vec::new(), pure_lets: Vec::new(), whole: true };
    scan.walk(root, false);
    let mut found = scan.found;
    // A worker runs none of the function's contract checks (#2425).
    if (0..body.exprs.len()).any(|i| in_contract(body, HirId(i as u32))) {
        for (_, site) in &mut found {
            site.guards.push(ParGuard::Opaque);
        }
    }
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

/// Depth-first body walk recording every fork site, tagged with whether it
/// sits on a return path (those win when picking the function's primary
/// site) and with the path condition that must hold to reach it.
struct Scan<'a> {
    ctx: &'a FnCtx<'a>,
    guards: Vec<ParGuard>,
    found: Vec<(bool, ParForkSite)>,
    /// Pure-call `let`s immediately preceding the current statement in this
    /// block, used to admit `let a = f(…); let b = g(…); return a ⊕ b`.
    pure_lets: Vec<(LocalId, ParArm)>,
    /// The expression being walked is all the function returns on this
    /// path: a `return`'s operand or the body's tail, not a part of
    /// another expression. A worker returns its site's combine, so a fork
    /// anywhere else is found but never specialized (#2425).
    whole: bool,
}

impl Scan<'_> {
    /// Walk `id` under one extra guard, then restore the guard stack.
    fn walk_guarded(&mut self, id: HirId, on_return: bool, guard: ParGuard) {
        let depth = self.guards.len();
        self.guards.push(guard);
        self.walk(id, on_return);
        self.guards.truncate(depth);
    }

    /// A fork found at the expression being walked.
    fn found(&mut self, on_return: bool, mut site: ParForkSite) {
        if !self.whole {
            site.guards.push(ParGuard::Opaque);
        }
        self.found.push((on_return, site));
    }

    /// Walk `id` as a part of another expression or statement.
    fn walk_part(&mut self, id: HirId, on_return: bool) {
        let whole = std::mem::replace(&mut self.whole, false);
        self.walk(id, on_return);
        self.whole = whole;
    }

    /// Statement list: each item may narrow the path condition for its
    /// successors. `tail` is the item whose value is the list's.
    fn walk_block(&mut self, items: &[HirId], tail: Option<HirId>, on_return: bool) {
        let ctx = self.ctx;
        let depth = self.guards.len();
        let whole = self.whole;
        // A nested block must not wipe the enclosing pending lets; only
        // this block's statements share a trailing-let list. A fork in it
        // names none of them, so its worker would not run their calls.
        let saved_lets = std::mem::take(&mut self.pure_lets);
        if !saved_lets.is_empty() {
            self.guards.push(ParGuard::Opaque);
        }
        for &item in items {
            self.whole = whole && tail == Some(item);
            self.walk(item, on_return);
            match diverging_if_negation(item, ctx) {
                Some(guard) => {
                    self.guards.push(guard);
                    self.pure_lets.clear();
                }
                // Any other statement that might return leaves the path
                // condition unknown for everything after it.
                None if contains_return(ctx, item) => {
                    self.guards.push(ParGuard::Opaque);
                    self.pure_lets.clear();
                }
                // And one that is no arm is work a fork after it would
                // skip: it may trap, or not return (#2425).
                None => {
                    if !self.note_pure_let(item) {
                        self.guards.push(ParGuard::Opaque);
                    }
                }
            }
        }
        self.whole = whole;
        self.guards.truncate(depth);
        self.pure_lets = saved_lets;
    }

    /// Track trailing `let name = pure_call(…)` so the next combine can name
    /// those arms; whether `item` is one. Any other statement drops the
    /// pending list: a fork worker re-emits only arms + combine, so prefix
    /// work would be skipped.
    fn note_pure_let(&mut self, item: HirId) -> bool {
        let ctx = self.ctx;
        let Some((local, arm)) = let_binding(ctx.body, item).and_then(|(local, init)| Some((local, pure_call_arm(init, ctx)?))) else {
            self.pure_lets.clear();
            return false;
        };
        // A rebound name replaces the binding it shadows.
        let name = &ctx.body.local(local).name;
        self.pure_lets.retain(|(l, _)| ctx.body.local(*l).name != *name);
        self.pure_lets.push((local, arm));
        true
    }

    /// Walk the operand of a `return`: all the function returns there.
    fn walk_returned(&mut self, value: HirId) {
        let whole = std::mem::replace(&mut self.whole, true);
        self.walk(value, true);
        self.whole = whole;
    }

    fn walk(&mut self, id: HirId, on_return: bool) {
        // Walks recurse through here once per level of the tree.
        common::deep_stack(|| self.walk_level(id, on_return))
    }

    fn walk_level(&mut self, id: HirId, on_return: bool) {
        let ctx = self.ctx;
        let body = ctx.body;
        let id = ctx.peel(id);
        if let Some(site) = match_fork(id, ctx, &self.guards, &self.pure_lets) {
            self.found(on_return, site);
        }
        if let Some(made) = made_match(body, id) {
            match made {
                Made::Try(inner) => self.walk_part(inner, on_return),
                Made::Coalesce(lhs, rhs) => {
                    self.walk_part(lhs, on_return);
                    self.walk_part(rhs, on_return);
                }
                Made::MainReturn(value) => self.walk_returned(value),
                Made::Other => {}
            }
            return;
        }
        match body.expr(id).kind.clone() {
            HirKind::Block { tail, .. } => self.walk_block(&ctx.items(id), tail, on_return),
            // A `let`'s initializer is a statement list of its own.
            HirKind::Let { init: Some(init), .. } => self.walk_block(&[init], None, on_return),
            // Else-if chain: every earlier condition failed for a later arm.
            HirKind::If { cond, then, els } => {
                let depth = self.guards.len();
                self.walk_part(cond, on_return);
                self.walk_guarded(then, on_return, guard_from_cond(cond, ctx, true));
                if let Some(els) = els {
                    self.guards.push(guard_from_cond(cond, ctx, false));
                    self.walk(els, on_return);
                }
                self.guards.truncate(depth);
            }
            HirKind::Return(_) => {
                if let Some(value) = returned(body, id) {
                    self.walk_returned(value);
                }
            }
            // Arms are scanned independently — a fork never spans two arms.
            // Constructor patterns are not evaluable from const int args, so
            // those bodies stay opaque, and so does every arm after one:
            // it runs only when that pattern missed (the `else` of an `if
            // let` is such an arm). Binding / wildcard / literal arms ahead
            // of any constructor inherit only the outer path guards.
            HirKind::Match { scrutinee, arms } => {
                self.walk_part(scrutinee, on_return);
                let mut missed = false;
                for arm in &arms {
                    match arm.pat {
                        HirPat::Wild | HirPat::Bind(_) | HirPat::Int(_) if !missed => self.walk(arm.body, on_return),
                        _ => {
                            missed = true;
                            self.walk_guarded(arm.body, on_return, ParGuard::Opaque);
                        }
                    }
                }
            }
            HirKind::Loop { body: loop_body } => {
                // A `while let` is not looked into.
                if let Some((_, cond, then)) = query::while_parts(body, loop_body) {
                    self.walk_part(cond, on_return);
                    self.walk_guarded(then, on_return, ParGuard::Opaque);
                }
            }
            HirKind::ForIn { iterable, .. } => {
                self.walk_part(iterable, on_return);
                if let Some((_, own)) = query::for_body_parts(body, id) {
                    self.walk_guarded(own, on_return, ParGuard::Opaque);
                }
            }
            _ => {
                for operand in operands(body, id) {
                    self.walk_part(operand, on_return);
                }
            }
        }
    }
}

/// `if cond { … return … }` with no `else`: everything after it runs with
/// `cond` false. `None` for statements that are not a diverging `if`.
fn diverging_if_negation(item: HirId, ctx: &FnCtx<'_>) -> Option<ParGuard> {
    let HirKind::If { cond, then, els: None } = *ctx.kind(item) else { return None };
    // The last statement of the body is a `return` / `raise`.
    let last = *ctx.items(then).last()?;
    written_return(ctx.body, last).then(|| guard_from_cond(cond, ctx, false))
}

/// Whether a statement can transfer control out of the enclosing function.
///
/// Covers the same nodes as [`Scan::walk`]; anything else is a leaf
/// expression that cannot hold a `return`.
fn contains_return(ctx: &FnCtx<'_>, id: HirId) -> bool {
    // Walks recurse through here once per level of the tree.
    common::deep_stack(|| contains_return_level(ctx, id))
}

fn contains_return_level(ctx: &FnCtx<'_>, id: HirId) -> bool {
    let body = ctx.body;
    let id = ctx.peel(id);
    if written_return(body, id) {
        return true;
    }
    let any = |ids: &[HirId]| ids.iter().any(|k| contains_return(ctx, *k));
    if let Some(made) = made_match(body, id) {
        return match made {
            Made::Try(inner) => contains_return(ctx, inner),
            Made::Coalesce(lhs, rhs) => any(&[lhs, rhs]),
            Made::MainReturn(_) | Made::Other => false,
        };
    }
    match &body.expr(id).kind {
        HirKind::Block { .. } => any(&ctx.items(id)),
        HirKind::Let { init: Some(init), .. } => contains_return(ctx, *init),
        HirKind::If { cond, then, els } => any(&[*cond, *then]) || els.is_some_and(|els| contains_return(ctx, els)),
        HirKind::Match { scrutinee, arms } => contains_return(ctx, *scrutinee) || arms.iter().any(|arm| contains_return(ctx, arm.body)),
        HirKind::Loop { body: loop_body } => query::while_parts(body, *loop_body).is_some_and(|(_, cond, then)| any(&[cond, then])),
        HirKind::ForIn { iterable, .. } => contains_return(ctx, *iterable) || query::for_body_parts(body, id).is_some_and(|(_, own)| contains_return(ctx, own)),
        _ => any(&operands(body, id)),
    }
}

/// Recognize `cond` as an integer comparison over parameters; anything else
/// is [`ParGuard::Opaque`] so the fork site is never specialized.
fn guard_from_cond(cond: HirId, ctx: &FnCtx<'_>, expect: bool) -> ParGuard {
    let (op, lhs, rhs) = match *ctx.kind(cond) {
        HirKind::Un { op: UnOp::Not | UnOp::BitNot, operand } => return guard_from_cond(operand, ctx, !expect),
        HirKind::Bin { op, lhs, rhs } => (op, lhs, rhs),
        _ => return ParGuard::Opaque,
    };
    let op = match source_op(op) {
        "<" => CmpOp::Lt,
        "<=" => CmpOp::Leq,
        ">" => CmpOp::Gt,
        ">=" => CmpOp::Geq,
        "==" => CmpOp::Eq,
        "!=" => CmpOp::Neq,
        _ => return ParGuard::Opaque,
    };
    match (guard_operand(lhs, ctx), guard_operand(rhs, ctx)) {
        (Some(lhs), Some(rhs)) => ParGuard::Cmp { lhs, op, rhs, expect },
        _ => ParGuard::Opaque,
    }
}

/// Comparison operand: an int literal or an int-like parameter (± a literal).
fn guard_operand(id: HirId, ctx: &FnCtx<'_>) -> Option<ArgForm> {
    let form = arg_form(id, ctx)?;
    match form {
        ArgForm::Param(i) => ctx.int_like(i).then_some(form),
        ArgForm::Const(_) | ArgForm::ParamMinus { .. } | ArgForm::ParamPlus { .. } => Some(form),
    }
}

/// Recognize the IPA fork shapes at `id` (no recursion into subtrees).
fn match_fork(id: HirId, ctx: &FnCtx<'_>, guards: &[ParGuard], lets: &[(LocalId, ParArm)]) -> Option<ParForkSite> {
    let (arms, combine) = match ctx.kind(id) {
        HirKind::Bin { op, lhs, rhs } => {
            let op = match source_op(*op) {
                "+" => ParBinOp::Add,
                "-" => ParBinOp::Sub,
                "*" => ParBinOp::Mul,
                "^" => ParBinOp::Xor,
                _ => return None,
            };
            let arms = if op == ParBinOp::Sub {
                vec![resolve_arm(*lhs, ctx, lets)?, resolve_arm(*rhs, ctx, lets)?]
            } else {
                let mut leaves = Vec::new();
                flatten_assoc(id, op, ctx, &mut leaves);
                resolve_arms(&leaves, ctx, lets)?
            };
            (arms, ParCombine::BinOp(op))
        }
        HirKind::Make { kind: MakeKind::Tuple, args } => (resolve_arms(args, ctx, lets)?, ParCombine::Tuple),
        HirKind::Make { kind: MakeKind::Variant { enum_name, variant, .. }, args } => {
            // A variant named bare (`Pair(f(a), f(b))`) is not taken: the
            // worker's combine spells the enum.
            let span = ctx.body.expr(id).span;
            if ctx.checker.bare_construct_at(span.0, span.1).is_some() {
                return None;
            }
            (resolve_arms(args, ctx, lets)?, ParCombine::EnumCtor { enum_name: enum_name.clone(), variant_name: variant.clone() })
        }
        HirKind::Call { args, .. } => {
            let name = bare_callee(ctx.checker, ctx.body, id)?;
            let arms = resolve_arms(args, ctx, lets)?;
            let combine = if name == ctx.fn_name {
                ParCombine::SelfCall
            } else if ctx.pure_fns.contains(name) {
                ParCombine::ApplyCall { fn_name: name.to_string() }
            } else {
                return None;
            };
            (arms, combine)
        }
        _ => return None,
    };
    (arms.len() >= 2).then(|| ParForkSite {
        fn_name: ctx.fn_name.to_string(),
        qualified: String::new(),
        param_count: ctx.params.len(),
        arms,
        combine,
        guards: guards.to_vec(),
    })
}

/// The operands of a chain of the same associative op, as written without
/// parentheses (`a + b + c`, which is `(a + b) + c`). The worker folds
/// them in that order, so each step is one the function takes: `+` and
/// `*` trap on overflow, and `a + (b + c)` can trap where `(a + b) + c`
/// does not. A right operand that is itself such a chain stays one
/// operand, which is no arm.
fn flatten_assoc(id: HirId, op: ParBinOp, ctx: &FnCtx<'_>, out: &mut Vec<HirId>) {
    let same = |found: &str| matches!((op, found), (ParBinOp::Add, "+") | (ParBinOp::Mul, "*") | (ParBinOp::Xor, "^"));
    // Down the left spine, then its right operands back up.
    let mut rights = Vec::new();
    let mut id = unwrap_byte(ctx.body, id);
    while let HirKind::Bin { op: found, lhs, rhs } = *ctx.kind(id)
        && same(source_op(found))
    {
        rights.push(unwrap_byte(ctx.body, rhs));
        id = unwrap_byte(ctx.body, lhs);
    }
    out.push(id);
    out.extend(rights.into_iter().rev());
}

/// Every operand must be an independent pure call (or a let bound to one):
/// the combine consumes arm results positionally, so a mixed operand list is
/// not representable. Named lets must be used exactly once and exhaust the
/// pending list — a reused binding is a loop-carried value, not two arms.
fn resolve_arms(items: &[HirId], ctx: &FnCtx<'_>, lets: &[(LocalId, ParArm)]) -> Option<Vec<ParArm>> {
    if items.len() < 2 {
        return None;
    }
    let read = |id: &HirId| match *ctx.kind(*id) {
        HirKind::Local(l) => Some(l),
        _ => None,
    };
    let mut used = HashSet::new();
    let mut arms = Vec::with_capacity(items.len());
    for item in items {
        if let Some(l) = read(item)
            && !used.insert(l)
        {
            return None;
        }
        arms.push(resolve_arm(*item, ctx, lets)?);
    }
    lets.iter().all(|(l, _)| used.contains(l)).then_some(arms)
}

fn resolve_arm(id: HirId, ctx: &FnCtx<'_>, lets: &[(LocalId, ParArm)]) -> Option<ParArm> {
    if let Some(arm) = pure_call_arm(id, ctx) {
        return Some(arm);
    }
    let HirKind::Local(local) = *ctx.kind(id) else { return None };
    lets.iter().find(|(l, _)| *l == local).map(|(_, arm)| arm.clone())
}

fn pure_call_arm(id: HirId, ctx: &FnCtx<'_>) -> Option<ParArm> {
    let HirKind::Call { args, .. } = ctx.kind(id) else { return None };
    let name = bare_callee(ctx.checker, ctx.body, id).filter(|name| ctx.pure_fns.contains(*name))?;
    // Arms of the enclosing specialization are keyed by the *enclosing*
    // function's params; each arm's forms must still parse under that ctx.
    let forms = args.iter().map(|a| arg_form(*a, ctx)).collect::<Option<Vec<_>>>()?;
    Some(ParArm::Call { callee: name.to_string(), args: forms, span: ctx.body.expr(id).span })
}

fn arg_form(id: HirId, ctx: &FnCtx<'_>) -> Option<ArgForm> {
    let param = |id: HirId| match *ctx.kind(id) {
        HirKind::Local(l) => ctx.param_index(l),
        _ => None,
    };
    let literal = |id: HirId| match *ctx.kind(id) {
        HirKind::Lit(Lit::Int(k)) => Some(k),
        _ => None,
    };
    let id = unwrap_byte(ctx.body, id);
    match *ctx.kind(id) {
        HirKind::Lit(Lit::Int(k)) => Some(ArgForm::Const(k)),
        HirKind::Local(_) => param(id).map(ArgForm::Param),
        HirKind::Bin { op, lhs, rhs } => match source_op(op) {
            "-" => {
                let (index, sub) = (param(lhs)?, literal(rhs)?);
                (sub > 0 && ctx.int_like(index)).then_some(ArgForm::ParamMinus { param: index, sub })
            }
            "+" => {
                let (index, add) = param(lhs).zip(literal(rhs)).or_else(|| param(rhs).zip(literal(lhs)))?;
                (add > 0 && ctx.int_like(index)).then_some(ArgForm::ParamPlus { param: index, add })
            }
            _ => None,
        },
        _ => None,
    }
}

// Constant call-site collection

/// The const calls of one function and the lambdas in it.
struct ConstCalls<'a> {
    module: &'a HirModule,
    checker: &'a Checker,
    sites: &'a HashMap<String, ParForkSite>,
    /// The function has a fork site: its calls are the site's own arms
    /// (`fib(n)` inside `pair_fib`).
    enclosing_fork: bool,
}

impl ConstCalls<'_> {
    fn collect(&self, body: &HirBody, id: HirId, work: &mut WorkEstimate<'_>, out: &mut HashMap<String, BTreeSet<Vec<i64>>>) {
        // Walks recurse through here once per level of the tree.
        common::deep_stack(|| self.collect_level(body, id, work, out))
    }

    fn collect_level(&self, body: &HirBody, id: HirId, work: &mut WorkEstimate<'_>, out: &mut HashMap<String, BTreeSet<Vec<i64>>>) {
        let id = unwrap_byte(body, id);
        let e = body.expr(id);
        // A contract's checks are not looked into.
        if in_contract(body, id) {
            return;
        }
        if let Some(made) = made_match(body, id) {
            let parts: &[HirId] = match &made {
                Made::Try(inner) | Made::MainReturn(inner) => std::slice::from_ref(inner),
                Made::Coalesce(lhs, rhs) => &[*lhs, *rhs],
                Made::Other => &[],
            };
            for part in parts {
                self.collect(body, *part, work, out);
            }
            return;
        }
        match &e.kind {
            HirKind::Block { stmts, tail } => {
                for item in stmts.iter().chain(tail) {
                    self.collect(body, *item, work, out);
                }
            }
            HirKind::Let { init: Some(init), .. } => self.collect(body, *init, work, out),
            HirKind::If { cond, then, els } => {
                for part in [Some(*cond), Some(*then), *els].into_iter().flatten() {
                    self.collect(body, part, work, out);
                }
            }
            HirKind::Return(_) => {
                if let Some(value) = returned(body, id) {
                    self.collect(body, value, work, out);
                }
            }
            HirKind::Match { scrutinee, arms } => {
                self.collect(body, *scrutinee, work, out);
                for arm in arms {
                    self.collect(body, arm.body, work, out);
                }
            }
            HirKind::Loop { body: loop_body } => {
                if let Some((_, cond, then)) = query::while_parts(body, *loop_body) {
                    self.collect(body, cond, work, out);
                    self.collect(body, then, work, out);
                }
            }
            HirKind::ForIn { iterable, body: own, .. } => {
                self.collect(body, *iterable, work, out);
                self.collect(body, *own, work, out);
            }
            HirKind::Defer { body: deferred, .. } => self.collect(body, *deferred, work, out),
            HirKind::Lambda { body: index } => {
                if let Some(lambda) = self.module.bodies.get(*index)
                    && let Some(root) = lambda.root
                {
                    self.collect(lambda, root, work, out);
                }
            }
            HirKind::Call { args, .. } => {
                for operand in operands(body, id) {
                    self.collect(body, operand, work, out);
                }
                if let Some(name) = bare_callee(self.checker, body, id) {
                    self.note_call(body, name, args, work, out);
                }
            }
            _ => {
                for operand in operands(body, id) {
                    self.collect(body, operand, work, out);
                }
            }
        }
    }

    fn note_call(&self, body: &HirBody, name: &str, args: &[HirId], work: &mut WorkEstimate<'_>, out: &mut HashMap<String, BTreeSet<Vec<i64>>>) {
        let Some(site) = self.sites.get(name) else { return };
        if args.len() != site.param_count {
            return;
        }
        let consts = args
            .iter()
            .map(|a| match body.expr(*a).kind {
                HirKind::Lit(Lit::Int(n)) => Some(n),
                _ => None,
            })
            .collect::<Option<Vec<_>>>();
        if let Some(consts) = consts {
            if work.worth_parallel(name, &consts) {
                out.entry(name.to_string()).or_default().insert(consts);
            }
            return;
        }
        // `fib(k)` demands the worker. Recursive `fib(n - 1)` is not a
        // name, so the sequential body does not re-enter.
        let named = |id: HirId| match &body.expr(id).kind {
            HirKind::Local(_) => true,
            HirKind::Global { name, .. } => !name.contains("::"),
            _ => false,
        };
        if !self.enclosing_fork
            && let [arg] = args
            && named(*arg)
            && let Some(cut) = unary_dynamic_cutoff(self.sites, name)
        {
            out.entry(name.to_string()).or_default().insert(vec![cut]);
        }
    }
}

#[cfg(test)]
#[path = "par_profit.tests.rs"]
mod tests;
