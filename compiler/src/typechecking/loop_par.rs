//! Independent Parallel Arms for counted loops (loop IPA).
//!
//! The recursive IPA in [`par_profit`](super::par_profit) treats sibling
//! self-calls as independent arms. A counted loop is the same idea with the
//! arms spread over an induction range: when the iterations only communicate
//! through one associative reduction `acc = acc ⊕ e(i)`, any partition of the
//! range folds to the sequential result.
//!
//! Detection is structural — no function, module or benchmark allowlists — and
//! fails closed. Anything the walk cannot prove independent (a nested branch, a
//! read of an enclosing local, an impure call, a second reduction) simply
//! leaves the loop sequential.

use std::collections::{HashMap, HashSet};

use parser::ast::{AdjustOp, AssignOp, Expression, Output};

use super::par_profit::par_cost_threshold;

/// Associative operator folding a loop's per-iteration contributions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopReduceOp {
    Add,
    Mul,
}

impl LoopReduceOp {
    /// Identity element. Every chunk but the first starts here, so folding the
    /// partials with `⊕` reproduces the sequential `acc`.
    pub fn identity(self) -> i64 {
        match self {
            Self::Add => 0,
            Self::Mul => 1,
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
    /// Pointer of the induction identifier's `Expression` (sidecar lookup).
    pub index_expr_ptr: usize,
    /// Pointer of `e` in `acc = acc ⊕ e`.
    pub reduce_expr_ptr: usize,
}

impl LoopParSite {
    pub fn trip_count(&self) -> i64 {
        self.end - self.begin
    }

    /// Split point of a 2-way chunking: `[begin, mid)` and `[mid, end)`.
    pub fn midpoint(&self) -> i64 {
        self.begin + self.trip_count() / 2
    }
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
    scan.walk(ast, &mut ConstLocals::new());
    scan.out
}

/// Locals proven to hold a compile-time int at the current program point.
type ConstLocals = HashMap<String, i64>;

struct Scan<'a> {
    pure_fns: &'a HashSet<String>,
    out: LoopParSites,
}

impl Scan<'_> {
    /// Statement list: each item may bind or invalidate a const local for the
    /// statements that follow it.
    fn walk_block(&mut self, items: &[Output<'_>], consts: &mut ConstLocals) {
        for item in items {
            self.walk(item, consts);
            for name in assigned_names(item) {
                consts.remove(&name);
            }
            if let Some((name, init)) = let_binding(item) {
                match int_literal(init) {
                    Some(k) => consts.insert(name.to_string(), k),
                    None => consts.remove(name),
                };
            }
        }
    }

    /// Walk a loop body that is *not* a fork site.
    ///
    /// Everything the loop assigns is dropped first: a const local's binding no
    /// longer describes every visit to a program point inside a loop.
    fn walk_loop_body(&mut self, loop_node: &Output<'_>, body: &Output<'_>, consts: &ConstLocals) {
        let mut inner = consts.clone();
        for name in assigned_names(loop_node) {
            inner.remove(&name);
        }
        self.walk(body, &mut inner);
    }

    fn walk(&mut self, ast: &Output<'_>, consts: &mut ConstLocals) {
        match ast.1.as_ref() {
            // A nested block's own bindings do not outlive it.
            Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
                self.walk_block(items, &mut consts.clone());
            }
            Expression::Module(_, inner)
            | Expression::Statement(inner)
            | Expression::Expr(inner)
            | Expression::ExprStatement(inner)
            | Expression::Group(inner)
            | Expression::Return(inner)
            | Expression::ImplicitReturn(inner) => self.walk(inner, consts),
            // Parameters are not const locals, so a body starts from nothing.
            Expression::Function {
                body: Some(body), ..
            } => self.walk(body, &mut ConstLocals::new()),
            Expression::Implementation { methods, .. } => {
                for m in methods {
                    self.walk(m, &mut ConstLocals::new());
                }
            }
            Expression::Method(_, inner) | Expression::Member(inner) => {
                self.walk(inner, &mut ConstLocals::new());
            }
            Expression::If(branches) => {
                for b in branches {
                    self.walk(b, &mut consts.clone());
                }
            }
            Expression::Branch(_, body) => self.walk(body, &mut consts.clone()),
            Expression::Match { arms, .. } => {
                for arm in arms {
                    self.walk(&arm.body, &mut consts.clone());
                }
            }
            Expression::Loop {
                identifier: None,
                iterable,
                body,
            } => match self.match_counted_loop(iterable, body, consts) {
                Some(site) => {
                    self.out.insert((ast.0.start, ast.0.end), site);
                }
                None => self.walk_loop_body(ast, body, consts),
            },
            Expression::Loop { body, .. } => {
                self.walk_loop_body(ast, body, consts);
            }
            _ => {}
        }
    }

    /// Match `while i < K { … }` against the loop-IPA shape.
    fn match_counted_loop(
        &self,
        cond: &Output<'_>,
        body: &Output<'_>,
        consts: &ConstLocals,
    ) -> Option<LoopParSite> {
        let (index, index_expr_ptr, bound, inclusive) = counted_bound(cond, consts)?;
        let begin = *consts.get(&index)?;
        let end = if inclusive {
            bound.checked_add(1)?
        } else {
            bound
        };
        // Profitability: the same cost cutoff the recursive IPA uses. A short
        // loop cannot pay for a spawn plus a join.
        if end.checked_sub(begin)? <= par_cost_threshold() {
            return None;
        }

        let items = block_items(body)?;
        let forms = items
            .iter()
            .map(|item| statement_form(peel(item), &index))
            .collect::<Option<Vec<_>>>()?;

        let mut acc_op: Option<(&str, LoopReduceOp, &Output<'_>)> = None;
        let mut steps = 0usize;
        for form in &forms {
            match form {
                StmtForm::Step => steps += 1,
                StmtForm::Reduce { acc, op, expr } => {
                    if acc_op.is_some() {
                        return None;
                    }
                    acc_op = Some((acc, *op, expr));
                }
                StmtForm::Local { .. } => {}
            }
        }
        if steps != 1 {
            return None;
        }
        let (acc, op, reduce_expr) = acc_op?;
        if acc == index {
            return None;
        }
        // The accumulator must be a const-initialized local of an enclosing
        // scope: that proves it is a frame slot codegen can find, and that no
        // earlier statement left it with an unknown value.
        if !consts.contains_key(acc) {
            return None;
        }

        // Every value the body computes must depend only on the induction
        // variable and temps declared earlier in the same body.
        let mut locals = HashSet::new();
        for form in &forms {
            match form {
                StmtForm::Local { name, init } => {
                    if *name == index || *name == acc || !self.independent(init, &index, acc, &locals)
                    {
                        return None;
                    }
                    locals.insert((*name).to_string());
                }
                StmtForm::Reduce { expr, .. } => {
                    if !self.independent(expr, &index, acc, &locals) {
                        return None;
                    }
                }
                StmtForm::Step => {}
            }
        }

        Some(LoopParSite {
            index,
            begin,
            end,
            acc: acc.to_string(),
            op,
            index_expr_ptr,
            reduce_expr_ptr: std::ptr::from_ref(reduce_expr) as *const Output<'_> as usize,
        })
    }

    /// Whether `expr` reads nothing but the induction variable, loop-private
    /// temps and integer literals, and calls nothing but pure functions.
    ///
    /// Deliberately narrow: division and modulo can trap, and every other node
    /// kind (index, field, method, lambda, `try`) either reaches shared state or
    /// cannot be re-emitted into a private frame.
    fn independent(
        &self,
        expr: &Output<'_>,
        index: &str,
        acc: &str,
        locals: &HashSet<String>,
    ) -> bool {
        let expr = peel(expr);
        let both = |a: &Output<'_>, b: &Output<'_>| {
            self.independent(a, index, acc, locals) && self.independent(b, index, acc, locals)
        };
        match expr.1.as_ref() {
            Expression::Integer(_) => true,
            Expression::Identifier(n) => *n != acc && (*n == index || locals.contains(*n)),
            Expression::Negate(a) | Expression::Positive(a) => {
                self.independent(a, index, acc, locals)
            }
            Expression::Add(a, b)
            | Expression::Sub(a, b)
            | Expression::Mul(a, b)
            | Expression::Shl(a, b)
            | Expression::Shr(a, b)
            | Expression::Xor(a, b)
            | Expression::BitAnd(a, b)
            | Expression::BitOr(a, b) => both(a, b),
            Expression::Call {
                name,
                args: Some(args),
            } => {
                let pure_callee = matches!(
                    peel(name).1.as_ref(),
                    Expression::Identifier(f) if self.pure_fns.contains(*f)
                );
                pure_callee && args.iter().all(|a| self.independent(a, index, acc, locals))
            }
            _ => false,
        }
    }
}

/// One admissible statement of a loop-IPA body.
enum StmtForm<'a> {
    /// `i = i + 1` / `i += 1` / `i++`.
    Step,
    /// `acc = acc ⊕ expr` / `acc ⊕= expr`.
    Reduce {
        acc: &'a str,
        op: LoopReduceOp,
        expr: &'a Output<'a>,
    },
    /// `let name = init` — a loop-private temp.
    Local { name: &'a str, init: &'a Output<'a> },
}

/// Classify one body statement; `None` rejects the whole loop.
fn statement_form<'a>(item: &'a Output<'a>, index: &str) -> Option<StmtForm<'a>> {
    if let Some((name, init)) = let_binding(item) {
        return Some(StmtForm::Local { name, init });
    }
    match item.1.as_ref() {
        Expression::Adjust {
            op: AdjustOp::Inc,
            target,
            ..
        } => (ident_name(target)? == index).then_some(StmtForm::Step),
        Expression::CompoundAssign(lhs, op, rhs) => {
            let name = ident_name(lhs)?;
            if name == index {
                return (*op == AssignOp::Add && int_literal(rhs) == Some(1))
                    .then_some(StmtForm::Step);
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
                let Expression::Add(a, b) = peel(rhs).1.as_ref() else {
                    return None;
                };
                return (ident_name(a) == Some(index) && int_literal(b) == Some(1))
                    .then_some(StmtForm::Step);
            }
            // `acc = acc ⊕ expr`: the accumulator must be the left operand, so
            // the reduction is the outermost node and `expr` is reduction-free.
            let (op, expr) = match peel(rhs).1.as_ref() {
                Expression::Add(a, b) => (LoopReduceOp::Add, (ident_name(a)? == name).then_some(b)?),
                Expression::Mul(a, b) => (LoopReduceOp::Mul, (ident_name(a)? == name).then_some(b)?),
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

fn compound_reduce_op(op: AssignOp) -> Option<LoopReduceOp> {
    match op {
        AssignOp::Add => Some(LoopReduceOp::Add),
        AssignOp::Mul => Some(LoopReduceOp::Mul),
        _ => None,
    }
}

/// `i < K` / `i <= K` with a compile-time `K`: `(index, index span, K, inclusive)`.
fn counted_bound(
    cond: &Output<'_>,
    consts: &ConstLocals,
) -> Option<(String, usize, i64, bool)> {
    let cond = peel(cond);
    let (lhs, rhs, inclusive) = match cond.1.as_ref() {
        Expression::Le(a, b) => (a, b, false),
        Expression::Leq(a, b) => (a, b, true),
        _ => return None,
    };
    let lhs = peel(lhs);
    let index = ident_name(lhs)?;
    let bound = match peel(rhs).1.as_ref() {
        Expression::Integer(k) => *k,
        Expression::Identifier(n) => *consts.get(*n)?,
        _ => return None,
    };
    Some((
        index.to_string(),
        std::ptr::from_ref(lhs) as *const Output<'_> as usize,
        bound,
        inclusive,
    ))
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
        let mut sites: Vec<LoopParSite> = analyze_loop_par_sites(&ast, &pure)
            .into_values()
            .collect();
        sites.sort_by(|a, b| a.begin.cmp(&b.begin));
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
        let t = par_cost_threshold();
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
    }

    #[test]
    fn rejects_dynamic_bound() {
        assert!(
            sites_of(
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
"#
            )
            .is_empty(),
            "a parameter bound is not a compile-time trip count"
        );
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
    fn rejects_outer_local_read() {
        assert!(
            sites_of(&program(
                r#"
    let acc = 0;
    let k = 7;
    let i = 0;
    while i < 100 {
        acc = acc + k;
        i = i + 1;
    }
"#
            ))
            .is_empty(),
            "the chunk worker runs in a private frame — no captures"
        );
    }

    #[test]
    fn rejects_branch_in_body() {
        assert!(
            sites_of(&program(
                r#"
    let acc = 0;
    let i = 0;
    while i < 100 {
        if i > 3 { acc = acc + 1; }
        i = i + 1;
    }
"#
            ))
            .is_empty(),
            "conditional bodies are out of the first slice"
        );
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
