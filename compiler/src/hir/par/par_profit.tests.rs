//! Which functions have a fork site, and which demand a worker.

use super::*;
use crate::hir::build_module;
use crate::typechecking::infer::Checker;

/// A checked module with its HIR and its pure functions.
struct Lowered {
    checker: Checker,
    module: HirModule,
    pure: HashSet<String>,
}

fn lower(src: &str) -> Lowered {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let (pure, _) = crate::hir::effects::lone_module_effects(&module, &checker);
    Lowered { checker, module, pure }
}

impl Lowered {
fn sites(&self) -> HashMap<String, ParForkSite> {
    self.module.par_fork_sites(&self.checker, &self.pure)
}

fn workers(&self, sites: &HashMap<String, ParForkSite>) -> HashSet<String> {
    self.module.par_worker_fns(&self.checker, sites)
}
}

fn sites_of(src: &str) -> HashMap<String, ParForkSite> {
    lower(src).sites()
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
    let m = lower(
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
    assert!(m.pure.contains("fib"));
    let sites = m.sites();
    let fib = sites.get("fib").expect("fib fork site");
    assert_eq!(fib.param_count, 1);
    assert_eq!(fib.combine, ParCombine::BinOp(ParBinOp::Add));
    assert_eq!(arm_args(fib, 0), [ArgForm::ParamMinus { param: 0, sub: 1 }]);
    assert_eq!(arm_args(fib, 1), [ArgForm::ParamMinus { param: 0, sub: 2 }]);

    let demanded = m.workers(&sites);
    assert!(
        demanded.contains("fib"),
        "fib(32) must demand one parameterized worker: {demanded:?}"
    );
    assert!(
        !demanded.contains("main"),
        "only the fork-site function is demanded: {demanded:?}"
    );
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
fn two_const_call_sites_are_independent_top_sites() {
    let m = lower(
        r#"
fn fib(int n) -> int {
    if n <= 1 { return n; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let a = fib(22);
    let b = fib(24);
    return;
}
"#,
    );
    let sites = m.sites();
    let demanded = m.workers(&sites);
    assert!(
        demanded.contains("fib"),
        "two const sites still share one worker: {demanded:?}"
    );
    assert_eq!(demanded.len(), 1, "one worker per function: {demanded:?}");
}

/// A const call under an `if let` demands its function's worker like one
/// anywhere else.
#[test]
fn a_const_call_under_if_let_demands_a_worker() {
    let m = lower(
        r#"
fn fib(int n) -> int {
    if n <= 1 { return n; }
    return fib(n - 1) + fib(n - 2);
}
fn pick(int n) -> Option<int> {
    if n > 0 { return Some(n); }
    return None;
}
fn main() {
    if let Some(v) = pick(3) {
        let a = fib(27);
    }
    return;
}
"#,
    );
    let sites = m.sites();
    assert!(m.workers(&sites).contains("fib"));
}

#[test]
fn below_threshold_const_does_not_demand_a_worker() {
    let m = lower(
        r#"
fn fib(int n) -> int {
    if n <= 1 { return n; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let a = fib(20);
    return;
}
"#,
    );
    let sites = m.sites();
    let demanded = m.workers(&sites);
    assert!(
        !demanded.contains("fib"),
        "arg at the grain floor must not demand a worker: {demanded:?}"
    );
    assert!(!args_worth_parallel(&sites, "fib", &[20]));
    assert!(args_worth_parallel(&sites, "fib", &[21]));
    assert!(!args_worth_parallel(&sites, "fib", &[]));
    assert!(!args_worth_parallel(&sites, "nosuch", &[21]));
    assert_eq!(unary_dynamic_cutoff(&sites, "fib"), Some(21));
}

#[test]
fn dynamic_unary_call_demands_one_worker() {
    let m = lower(
        r#"
fn fib(int n) -> int {
    if n <= 1 { return n; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let k = 22;
    let b = fib(k);
    return;
}
"#,
    );
    let sites = m.sites();
    let demanded = m.workers(&sites);
    assert!(
        demanded.contains("fib"),
        "a dynamic unary call demands the parameterized worker: {demanded:?}"
    );
}

/// Grain is `W` itself. For `n <= 1` fib, `W(n) = Fib(n+1) - 1`, and
/// `W(20) = DEFAULT_EXPR_GRAIN` so the floor still admits `fib(21)`.
#[test]
fn fib_shape_scores_fork_tree_grain() {
    let floor = DEFAULT_EXPR_GRAIN;
    let sites = sites_of(
        r#"
fn fib(int n) -> int {
    if n <= 1 { return n; }
    return fib(n - 1) + fib(n - 2);
}
fn main() { return; }
"#,
    );
    assert_eq!(par_work_grain(&sites, "fib", &[2]), 1);
    assert_eq!(par_work_grain(&sites, "fib", &[5]), 7);
    assert_eq!(par_work_grain(&sites, "fib", &[20]), 10_945);
    assert_eq!(par_work_grain(&sites, "fib", &[20]), floor);
    // Above the floor the count saturates — the verdict cannot change.
    assert_eq!(par_work_grain(&sites, "fib", &[21]), floor + 1);
    assert_eq!(par_work_grain(&sites, "fib", &[32]), floor + 1);
    assert!(args_worth_parallel(&sites, "fib", &[32]));
    assert!(!args_worth_parallel(&sites, "fib", &[20]));
    assert!(args_worth_parallel(&sites, "fib", &[21]));
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
    // One fork-site node; helper callees contribute 0.
    assert_eq!(par_work_grain(&sites, "pair_sq", &[22]), 1);
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
/// so `W` is a lower bound (**8398** for the fair load). That sits below
/// the tight fib floor **10945** and above the loose floor **8000**.
#[test]
fn fair_tak_load_uses_loose_grain() {
    let tight = DEFAULT_EXPR_GRAIN;
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
    let tak = sites.get("tak").expect("tak fork");
    assert!(!grain_w_is_tight(tak));
    assert_eq!(par_expr_grain_for(tak), DEFAULT_LOOSE_EXPR_GRAIN);
    assert_eq!(par_work_grain(&sites, "tak", &[18, 12, 6]), 8398);
    assert!(
        8398 < tight,
        "fair tak W must sit below the tight fib floor"
    );
    assert!(
        args_worth_parallel(&sites, "tak", &[18, 12, 6]),
        "loose grain admits the fair tak(18, 12, 6) load"
    );
    // `max(args)` alone rated this above the floor; it is 53 calls.
    assert!(
        !args_worth_parallel(&sites, "tak", &[24, 22, 20]),
        "a narrow x - y gap is cheap however large the args"
    );
    assert!(args_worth_parallel(&sites, "tak", &[21, 12, 6]));
    assert!(args_worth_parallel(&sites, "tak", &[24, 16, 8]));
}

#[test]
fn fib_shape_keeps_tight_grain() {
    let sites = sites_of(
        r#"
fn fib(int n) -> int {
    if n <= 1 { return n; }
    return fib(n - 1) + fib(n - 2);
}
fn main() { return; }
"#,
    );
    let fib = sites.get("fib").expect("fib fork");
    assert!(grain_w_is_tight(fib));
    assert_eq!(par_expr_grain_for(fib), DEFAULT_EXPR_GRAIN);
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
    let _ = par_work_grain(&sites, "ping", &[40, 3]);
}

/// Top-site policy: only the const call is demanded, not the arm closure.
/// `f(z - 1, x, y)` would otherwise fan out combinatorially.
#[test]
fn tak_top_site_does_not_close_arm_children() {
    let m = lower(
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
    let sites = m.sites();
    let demanded = m.workers(&sites);
    assert!(
        demanded.contains("tak"),
        "root call site must demand the tak worker"
    );
    assert_eq!(
        demanded.len(),
        1,
        "tak must not grow a per-arg constellation: {demanded:?}"
    );
}

/// A clone has no base case, so arg vectors that take the early return
/// must never be specialized (`tak(0, 0, 21)` returns `z` immediately).
#[test]
fn base_case_arg_vectors_are_not_specialized() {
    let m = lower(
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
    let sites = m.sites();
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
    let demanded = m.workers(&sites);
    assert!(
        demanded.contains("tak"),
        "profitable tak(21, 12, 6) still demands a worker"
    );
}

/// An early return the analysis cannot evaluate must disable the site.
#[test]
fn opaque_guard_blocks_specialization() {
    let m = lower(
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
    let sites = m.sites();
    assert_eq!(
        sites.get("f").map(|s| s.guards.as_slice()),
        Some(&[ParGuard::Opaque][..])
    );
    let demanded = m.workers(&sites);
    assert!(
        !demanded.contains("f"),
        "unevaluable guard must block the worker: {demanded:?}"
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

/// #1394: a bare unit case arm (`None`) is a constructor pattern, not
/// an irrefutable binding, so a fork inside it is opaque.
#[test]
fn bare_unit_case_arm_is_opaque() {
    let sites = sites_of(
        r#"
fn pick(int n) -> Option<int> {
    if n <= 1 { return Option::Some(n); }
    return Option::None;
}
fn fibm(int n) -> int {
    return match pick(n) {
        None => fibm(n - 1) + fibm(n - 2),
        Some(v) => v,
    };
}
fn main() { return; }
"#,
    );
    let fibm = sites.get("fibm").expect("fibm fork site");
    assert!(
        fibm.guards.iter().any(|g| matches!(g, ParGuard::Opaque)),
        "a `None` arm is refutable: {:?}",
        fibm.guards
    );
}

#[test]
fn worker_names_are_per_site_not_per_arg() {
    assert_eq!(par_worker_name("fib"), "__coil_par_fib");
    assert_eq!(par_worker_name("tak"), "__coil_par_tak");
}

/// Nested associative `+` flattens to N independent arms (tribonacci).
#[test]
fn detects_nary_add_fork() {
    let sites = sites_of(
        r#"
fn trib(int n) -> int {
    if n <= 2 { return n; }
    return trib(n - 1) + trib(n - 2) + trib(n - 3);
}
fn main() { return; }
"#,
    );
    let trib = sites.get("trib").expect("trib fork site");
    assert_eq!(trib.combine, ParCombine::BinOp(ParBinOp::Add));
    assert_eq!(trib.arms.len(), 3);
    assert_eq!(
        arm_args(trib, 2),
        [ArgForm::ParamMinus { param: 0, sub: 3 }]
    );
    assert!(args_worth_parallel(&sites, "trib", &[22]));
    assert!(!args_worth_parallel(&sites, "trib", &[10]));
}

/// A chain is its operands as written: `a + (b + c)` adds in another
/// order than `a + b + c`, and `+` traps on overflow, so it is no
/// three-arm site (the inner sum is not all the function returns).
#[test]
fn a_parenthesized_right_operand_is_one_operand() {
    let sites = sites_of(
        r#"
fn trib(int n) -> int {
    if n <= 2 { return n; }
    return trib(n - 1) + (trib(n - 2) + trib(n - 3));
}
fn left(int n) -> int {
    if n <= 2 { return n; }
    return (left(n - 1) + left(n - 2)) + left(n - 3);
}
fn main() { return; }
"#,
    );
    let specializes = |name: &str| sites.get(name).is_some_and(|s| !s.guards.contains(&ParGuard::Opaque));
    assert!(!specializes("trib"), "{:?}", sites.get("trib"));
    assert!(specializes("left"));
    assert_eq!(sites["left"].arms.len(), 3);
}

#[test]
fn detects_xor_binop_fork() {
    let sites = sites_of(
        r#"
fn mix(int n) -> int {
    if n <= 1 { return n; }
    return mix(n - 1) ^ mix(n - 2);
}
fn main() { return; }
"#,
    );
    let mix = sites.get("mix").expect("xor fork site");
    assert_eq!(mix.combine, ParCombine::BinOp(ParBinOp::Xor));
    assert_eq!(mix.arms.len(), 2);
}

/// `let a = f(…); let b = g(…); return a + b` is the same two arms.
#[test]
fn detects_let_bound_independent_arms() {
    let sites = sites_of(
        r#"
fn fib(int n) -> int {
    if n <= 1 { return n; }
    let a = fib(n - 1);
    let b = fib(n - 2);
    return a + b;
}
fn main() { return; }
"#,
    );
    let fib = sites.get("fib").expect("let-bound fib fork");
    assert_eq!(fib.combine, ParCombine::BinOp(ParBinOp::Add));
    assert_eq!(fib.arms.len(), 2);
    assert_eq!(arm_args(fib, 0), [ArgForm::ParamMinus { param: 0, sub: 1 }]);
    assert_eq!(arm_args(fib, 1), [ArgForm::ParamMinus { param: 0, sub: 2 }]);
    assert!(args_worth_parallel(&sites, "fib", &[21]));
}

/// Reusing a let is one value, not two independent calls.
#[test]
fn rejects_let_bound_reused_name() {
    let sites = sites_of(
        r#"
fn fib(int n) -> int {
    if n <= 1 { return n; }
    let a = fib(n - 1);
    return a + a;
}
fn main() { return; }
"#,
    );
    assert!(
        !sites.contains_key("fib"),
        "a + a must not fork the same let twice: {sites:?}"
    );
}

/// Work between the lets and the combine would be dropped by the worker.
#[test]
fn rejects_let_bound_with_intervening_stmt() {
    let sites = sites_of(
        r#"
fn bump(int n) -> int { return n + 1; }
fn fib(int n) -> int {
    if n <= 1 { return n; }
    let a = fib(n - 1);
    let _ = bump(n);
    let b = fib(n - 2);
    return a + b;
}
fn main() { return; }
"#,
    );
    assert!(
        !sites.contains_key("fib"),
        "intervening stmt must refuse let-bound IPA: {sites:?}"
    );
}

/// `k + 1` is a structural arg form, not an allowlist.
#[test]
fn detects_param_plus_arm() {
    let sites = sites_of(
        r#"
fn walk(int n, int k) -> int {
    if n <= 0 { return k; }
    return walk(n - 1, k) + walk(n - 1, k + 1);
}
fn main() { return; }
"#,
    );
    let walk = sites.get("walk").expect("param-plus fork");
    assert_eq!(walk.combine, ParCombine::BinOp(ParBinOp::Add));
    assert_eq!(
        arm_args(walk, 1),
        [
            ArgForm::ParamMinus { param: 0, sub: 1 },
            ArgForm::ParamPlus { param: 1, add: 1 }
        ]
    );
    assert!(args_worth_parallel(&sites, "walk", &[21, 0]));
}

/// Subtraction does not flatten: `(a-b)-c` is not `a-b-c` as three arms.
#[test]
fn sub_does_not_flatten_nary() {
    let sites = sites_of(
        r#"
fn diff(int n) -> int {
    if n <= 2 { return n; }
    return diff(n - 1) - diff(n - 2) - diff(n - 3);
}
fn main() { return; }
"#,
    );
    let diff = sites.get("diff").expect("sub fork");
    assert_eq!(diff.combine, ParCombine::BinOp(ParBinOp::Sub));
    assert_eq!(
        diff.arms.len(),
        2,
        "non-associative sub must stay binary: {diff:?}"
    );
}

/// The site of `f` in `fn f(int n) -> ret { body }`, with pure `fib`, `sq`,
/// `maybe` and `checked` and an enum `Pair` at hand.
fn f_site(ret: &str, body: &str) -> Option<ParForkSite> {
    let src = format!(
        r#"
enum Pair {{ Both(int, int), One(int) }}
fn fib(int n) -> int {{
    if n <= 1 {{ return n; }}
    return fib(n - 1) + fib(n - 2);
}}
fn sq(int n) -> int {{ return n * n; }}
fn maybe(int n) -> Option<int> {{ return Some(n); }}
fn checked(int n) -> Result<int, string> {{ return Ok(n); }}
fn f(int n) -> {ret} {{
{body}
}}
fn main() {{ return; }}
"#
    );
    let mut m = lower(&src);
// As the effect summaries prove them: all pure.
    m.pure = ["fib", "sq", "maybe", "checked", "f"].map(String::from).into();
    m.sites().remove("f")
}

/// Parentheses change nothing: a group is the expression in it.
#[test]
fn parentheses_do_not_hide_let_bound_arms() {
    let opaque = |body: &str| f_site("int", body).expect("site").guards.contains(&ParGuard::Opaque);
    // The group is the fork itself: the pending lets are its arms.
    let site = f_site("int", "let a = fib(n - 1); let b = fib(n - 2); return (a + b);").expect("site");
    assert_eq!(site.arms.len(), 2);
    assert!(!site.guards.contains(&ParGuard::Opaque));
    // Below a group they are arms too, of a fork that is a part of
    // another expression and so never specialized (#2425).
    assert!(opaque("let a = fib(n - 1); let b = fib(n - 2); if !(a - b == 0) { return 1; } return 2;"));
    assert!(opaque("let a = fib(n - 1); let b = fib(n - 2); if a - b == 0 { return 1; } return 2;"));
    // A fork that leaves a pending let unused is none, in a group or not.
    assert!(f_site("int", "let a = fib(n - 1); return fib(n - 1) + fib(n - 2);").is_none());
    assert!(f_site("int", "let a = fib(n - 1); return 1 * (fib(n - 1) + fib(n - 2));").is_none());
}

#[test]
fn a_variant_named_bare_is_no_combine() {
    let site = f_site("Pair", "return Pair::Both(fib(n - 1), fib(n - 2));").expect("site");
    assert_eq!(site.combine, ParCombine::EnumCtor { enum_name: "Pair".into(), variant_name: "Both".into() });
    assert!(f_site("Pair", "return Both(fib(n - 1), fib(n - 2));").is_none());
}

/// An `if let` is a `match`: a fork in an arm that runs only when a
/// constructor pattern matched, or missed, is found and never specialized
/// (a worker's guards compare `int` parameters, and cannot test either).
#[test]
fn a_fork_under_if_let_is_never_specialized() {
    let opaque = |body: &str| f_site("int", body).expect("site").guards.contains(&ParGuard::Opaque);
    assert!(opaque("if let Some(v) = maybe(n) { return fib(n - 1) + fib(n - 2); } return 0;"));
    assert!(opaque("if let Some(v) = maybe(n) { return v; } else { return fib(n - 1) + fib(n - 2); }"));
    assert!(opaque("match maybe(n) { Some(v) => { return v; }, default => { return fib(n - 1) + fib(n - 2); }, }"));
    // It is a statement a fork after it would skip, like any other (#2425).
    let site = f_site("int", "if let Some(v) = maybe(n) { return v; } return fib(n - 1) + fib(n - 2);").expect("site");
    assert_eq!(site.guards, [ParGuard::Opaque]);
    let site = f_site("int", "match maybe(n) { Some(v) => { return v; }, None => {}, } return fib(n - 1) + fib(n - 2);").expect("site");
    assert_eq!(site.guards, [ParGuard::Opaque]);
}

#[test]
fn a_try_is_its_operand() {
// `?` returns early, which the walk does not count as a `return`: its
// `let` is a statement the fork after it would skip.
    let site = f_site("Result<int, string>", "let k = checked(n)?; return fib(n - 1) + fib(n - 2);").expect("site");
    assert!(site.guards.contains(&ParGuard::Opaque), "{:?}", site.guards);
// A fork under `??` is found, and is not what the function returns.
    let site = f_site("int", "let k = maybe(n) ?? (fib(n - 1) + fib(n - 2)); return k;").expect("site");
    assert!(site.guards.contains(&ParGuard::Opaque), "{:?}", site.guards);
}

#[test]
fn a_fork_is_all_its_function_returns_or_never_specialized() {
    let clear = |body: &str| !f_site("int", body).expect("site").guards.contains(&ParGuard::Opaque);
    // The operand of a `return`, in a group or under an `if` too.
    assert!(clear("return fib(n - 1) + fib(n - 2);"));
    assert!(clear("return (fib(n - 1) + fib(n - 2));"));
    assert!(clear("if n > 40 { return 0; } let a = fib(n - 1); let b = fib(n - 2); return a + b;"));
    assert!(clear("if n <= 2 { return 1; } else { return fib(n - 1) + fib(n - 2); }"));
    // A part of a larger expression, a call's argument, a `let`.
    assert!(!clear("return fib(n - 1) + fib(n - 2) + n;"));
    assert!(!clear("return sq(fib(n - 1) + fib(n - 2));"));
    assert!(!clear("let both = fib(n - 1) + fib(n - 2); return both + 1;"));
    assert!(!clear("fib(n - 1) + fib(n - 2); return 0;"));
    // A statement ahead of it that is neither a guard nor one of its arms.
    assert!(!clear("let d = 100 / (n - 27); return fib(n - 1) + fib(n - 2);"));
    assert!(!clear("let t = 0; for i in 0..3 { t = t + i; } return fib(n - 1) + fib(n - 2);"));
    assert!(!clear("let a = fib(n - 3); if n > 40 { return fib(n - 1) + fib(n - 2); } return a;"));
}

#[test]
fn contract_checks_are_not_part_of_the_body() {
    let src = r#"
fn fib(int n) -> int {
    if n <= 1 { return n; }
    return fib(n - 1) + fib(n - 2);
}
fn f(int n) -> int
    requires n >= 0
    ensures result >= fib(n - 1) - fib(n - 2)
{
    let a = fib(n - 1);
    let b = fib(n - 2);
    return a + b;
}
fn g(int n) -> int
    requires fib(n - 1) + fib(n - 2) > 0
{
    return n;
}
fn main() { return; }
"#;
    let m = lower(src);
    let sites = m.sites();
    let f = sites.get("f").expect("site");
    assert_eq!(f.combine, ParCombine::BinOp(ParBinOp::Add));
    // The site is found past the checks, and never specialized: a worker
    // would run none of them (#2425).
    assert_eq!(f.guards, [ParGuard::Opaque]);
    assert!(!sites.contains_key("g"));
}

#[test]
fn a_method_takes_its_declared_parameters() {
    let src = r#"
fn fib(int n) -> int {
    if n <= 1 { return n; }
    return fib(n - 1) + fib(n - 2);
}
class C { pub k: int, }
impl C {
    pub fn both(int n) -> int {
        return fib(n - 1) + fib(n - 2);
    }
}
trait Tot<A> {
    fn tot(A self, int n) -> int {}
}
impl Tot for C {
    fn tot(C self, int n) -> int {
        return fib(n - 1) + fib(n - 2);
    }
}
fn through_trait(C c, int n) -> int {
    return Tot::tot(c, n) + Tot::tot(c, n);
}
fn main() { return; }
"#;
    let m = lower(src);
    let sites = m.sites();
    let both = sites.get("both").expect("an inherent method's site");
    assert_eq!(both.param_count, 1);
    assert_eq!(arm_args(both, 0), [ArgForm::ParamMinus { param: 0, sub: 1 }]);
// A trait's method is not looked at, and a call through the trait is
// no arm: the bare name it lowers under is no function's.
    assert!(!sites.contains_key("tot"));
    assert!(!sites.contains_key("through_trait"));
}
