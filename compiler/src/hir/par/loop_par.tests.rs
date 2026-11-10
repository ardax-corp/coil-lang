//! Which counted loops are fork sites.

use super::*;
use crate::hir::build_module;
use crate::typechecking::infer::Checker;

fn sites_of(src: &str) -> Vec<LoopParSite> {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let (pure, _) = crate::hir::effects::lone_module_effects(&module, &checker);
    let mut sites: Vec<LoopParSite> = module.loop_par_sites(&checker, "", &pure).into_values().collect();
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
    let t = DEFAULT_LOOP_GRAIN;
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
fn admits_flat_pixel_scan_with_literal_divisors() {
    let site = one_site(
        r#"
fn pixel(int x, int y) -> int {
    return x + y;
}
fn mandelbrot() -> int {
    let sum = 0;
    let i = 0;
    while i < 25600 {
        let x = i % 160;
        let y = i / 160;
        sum = sum + pixel(x, y);
        i = i + 1;
    }
    return sum;
}
fn main() { return; }
"#,
    );
    assert_eq!(site.index, "i");
    assert_eq!(site.acc, "sum");
    assert_eq!(site.trip_count(), 25600);
    assert!(!site.is_dynamic());
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
        acc = acc ^ k;
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

#[test]
fn admits_a_for_loop_under_checks_of_what_it_leaves_alone() {
    let site = one_site(&program(
        r#"
    let k = 3;
    let acc = 0;
    for x in 0..100 invariant k > 0 {
        acc = acc + sq(x) * k;
    }
"#,
    ));
    assert!(site.implicit_step);
    // What only the check reads is the worker's too.
    let site = one_site(
        r#"
fn sq(int i) -> int { return i * i; }
fn run(int n, int m) -> int {
    let acc = 0;
    for x in 0..n invariant m > 0 {
        acc = acc + sq(x);
    }
    return acc;
}
fn main() { return; }
"#,
    );
    assert_eq!(site.live_captures, ["m"]);
}

/// A chunk worker runs a `for` loop's checks with its body: one that reads
/// what a body may not keeps the loop sequential.
#[test]
fn a_check_a_worker_cannot_run_keeps_a_for_loop_sequential() {
    for (param, clause) in [("byte m", "m == m"), ("[int] m", "m.len() > 0"), ("float m", "m > 0.0")] {
        let sites = sites_of(&format!(
            r#"
fn sq(int i) -> int {{ return i * i; }}
fn run(int n, {param}) -> int {{
    let acc = 0;
    for x in 0..n invariant {clause} {{
        acc = acc + sq(x);
    }}
    return acc;
}}
fn main() {{ return; }}
"#
        ));
        assert!(sites.is_empty(), "{clause}: {sites:?}");
    }
}

/// A `while` runs its checks ahead of every test, the one that ends it
/// too; the plan builds no worker that does (#2450).
#[test]
fn a_checked_while_loop_is_sequential() {
    let sites = sites_of(
        r#"
fn sq(int i) -> int { return i * i; }
fn run(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n invariant n >= 0 {
        acc = acc + sq(i);
        i = i + 1;
    }
    return acc;
}
fn main() { return; }
"#,
    );
    assert!(sites.is_empty(), "{sites:?}");
}

/// The fork-join's words are `int` locals: a `byte` is none (#2450).
#[test]
fn a_byte_word_keeps_the_loop_sequential() {
    let run = |params: &str, init: &str, body: &str| {
        sites_of(&format!(
            r#"
fn sq(int i) -> int {{ return i * i; }}
fn run({params}) -> int {{
    let acc = 0;
    {init}
    while i < n {{
        {body}
        i = i + 1;
    }}
    return acc;
}}
fn main() {{ return; }}
"#
        ))
    };
    assert_eq!(run("int n, int w", "let i = 0;", "acc = acc ^ (sq(i) * w);").len(), 1);
    // A live capture.
    let sites = run("int n, byte w", "let i = 0;", "if w == w { acc = acc + sq(i); }");
    assert!(sites.is_empty(), "{sites:?}");
    // The bound, and with it the index.
    let sites = run("byte n", "let i = n & n;", "acc = acc + 1;");
    assert!(sites.is_empty(), "{sites:?}");
}

/// A worker takes its bounds, the accumulator and each live capture as one
/// spawn's arguments.
#[test]
fn more_live_captures_than_a_spawn_passes_keep_the_loop_sequential() {
    let run = |captures: usize| {
        let names: Vec<String> = (0..captures).map(|i| format!("c{i}")).collect();
        let params: Vec<String> = names.iter().map(|n| format!("int {n}")).collect();
        sites_of(&format!(
            r#"
fn run(int n, {}) -> int {{
    let acc = 0;
    for x in 0..n {{
        acc = acc ^ (x * ({}));
    }}
    return acc;
}}
fn main() {{ return; }}
"#,
            params.join(", "),
            names.join(" + ")
        ))
    };
    let most = common::MAX_THREAD_SPAWN_ARGS - 3;
    assert_eq!(run(most).len(), 1);
    assert!(run(most + 1).is_empty());
}

#[test]
fn looks_in_inherent_methods_and_match_arms() {
    let sites = sites_of(
        r#"
fn sq(int i) -> int { return i * i; }
class C { pub k: int, }
impl C {
    pub fn sum() -> int {
        let acc = 0;
        for x in 0..100 { acc = acc + sq(x); }
        return acc;
    }
}
fn pick(Option<int> o) -> int {
    let acc = 0;
    match o {
        Some(v) => { for x in 0..50 { acc = acc + sq(x); } },
        None => {},
    }
    return acc;
}
fn main() { return; }
"#,
    );
    let mut ends: Vec<i64> = sites.iter().map(|s| s.end).collect();
    ends.sort_unstable();
    assert_eq!(ends, [50, 100]);
}

/// The ends of the sites of a program with one loop in each place the AST
/// walk this analysis replaced did not look: a trait's default body
/// (`101`), an instance's method (`102`), under `if let` (`103`) and
/// `while let` (`106`), up to a local `const` (`104`), and with no name
/// for its index (`105`).
fn reach() -> Vec<i64> {
    let sites = sites_of(
        r#"
fn sq(int i) -> int { return i * i; }
class C { pub k: int, }
trait Tot<A> {
    fn tot(A self) -> int {}
    fn twice(A self) -> int {
        let acc = 0;
        for x in 0..101 { acc = acc + sq(x); }
        return acc;
    }
}
impl Tot for C {
    fn tot(C self) -> int {
        let acc = 0;
        for x in 0..102 { acc = acc + sq(x); }
        return acc;
    }
}
fn under_if_let(Option<int> o) -> int {
    let acc = 0;
    if let Some(v) = o { for x in 0..103 { acc = acc + sq(x); } }
    return acc;
}
fn const_bound() -> int {
    const N = 104;
    let acc = 0;
    for x in 0..N { acc = acc + sq(x); }
    return acc;
}
fn unnamed_index(int k) -> int {
    let acc = 0;
    for _ in 0..105 { acc = acc ^ k; }
    return acc;
}
fn next(int at) -> Option<int> {
    if at < 3 { return Some(at); }
    return None;
}
fn under_while_let() -> int {
    let total = 0;
    let at = 0;
    while let Some(v) = next(at) {
        let acc = 0;
        for x in 0..106 { acc = acc + sq(x); }
        total = total + acc + v;
        at = at + 1;
    }
    return total;
}
fn main() { return; }
"#,
    );
    let mut ends: Vec<i64> = sites.iter().map(|s| s.end).collect();
    ends.sort_unstable();
    ends
}

/// A local `const` bounds a loop as a `let` of the same value does.
#[test]
fn a_local_const_bounds_a_loop() {
    assert!(reach().contains(&104), "{:?}", reach());
}

/// `Trait::m(x)` lowers under the bare method name: no call of a function.
#[test]
fn a_call_through_a_trait_is_no_pure_call() {
    let src = r#"
trait Weigh<A> {
    fn weigh(A self, int k) -> int {}
}
impl Weigh for int {
    fn weigh(int x, int k) -> int { return x * k; }
}
fn weigh2(int x, int k) -> int { return x * k; }
fn through() -> int {
    let acc = 0;
    for x in 0..100 { acc = acc + Weigh::weigh(x, 2); }
    return acc;
}
fn bare() -> int {
    let acc = 0;
    for x in 0..50 { acc = acc + weigh2(x, 2); }
    return acc;
}
fn main() { return; }
"#;
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let pure = ["weigh", "weigh2"].map(String::from).into();
    let sites: Vec<LoopParSite> = module.loop_par_sites(&checker, "", &pure).into_values().collect();
    assert_eq!(sites.iter().map(|s| s.end).collect::<Vec<_>>(), [50]);
}

/// The sites of `body` with every contract check built in.
fn checked_sites(body: &str) -> Vec<LoopParSite> {
    crate::hir::set_contract_level(crate::hir::ContractLevel::All);
    sites_of(&program(body))
}

#[test]
fn an_invariant_over_the_accumulator_keeps_a_for_loop_sequential() {
    let sites = checked_sites(
        r#"
    let acc = 0;
    for x in 0..100
        invariant acc >= 0
    {
        acc = acc + sq(x);
    }
"#,
    );
    assert!(sites.is_empty(), "a chunk checks the clause against its partial fold: {sites:?}");
}

#[test]
fn an_invariant_over_what_a_while_loop_writes_keeps_it_sequential() {
    for clause in ["invariant i <= n", "invariant acc >= 0", "decreases n - i"] {
        crate::hir::set_contract_level(crate::hir::ContractLevel::All);
        let sites = sites_of(&format!(
            r#"
fn sq(int i) -> int {{ return i * i; }}
fn run(int n) -> int {{
    let acc = 0;
    let i = 0;
    while i < n
        {clause}
    {{
        acc = acc + sq(i);
        i = i + 1;
    }}
    return acc;
}}
fn main() {{ return; }}
"#
        ));
        assert!(sites.is_empty(), "`{clause}`: {sites:?}");
    }
}

/// The sites of `run`, a fold of `term` onto `acc` from `start` by `op`
/// over `range`, with `params`.
fn fold_sites(params: &str, start: &str, range: &str, op: &str, term: &str) -> Vec<LoopParSite> {
    sites_of(&format!(
        r#"
fn sq(int i) -> int {{ return i * i; }}
fn weight(int i) -> int {{ return i % 7; }}
fn abs(int x) -> int {{ if x < 0 {{ return 0 - x; }} return x; }}
fn step(int i) -> int {{ if i < 10 {{ return 10; }} return 0 - 1; }}
fn run({params}) -> int {{
    let acc = {start};
    for i in {range} {{
        acc = acc {op} ({term});
    }}
    return acc;
}}
fn main() {{ return; }}
"#
    ))
}

/// The run-time tests of each site [`fold_sites`] finds, as written
/// (`k >= 1`): none for a site whose fold has one sign whatever the
/// values.
fn fold_guards(params: &str, start: &str, range: &str, op: &str, term: &str) -> Vec<Vec<String>> {
    fold_sites(params, start, range, op, term).iter().map(|s| s.guard.iter().map(ToString::to_string).collect()).collect()
}

/// A chunk folds from the identity, so it cannot see the running sum
/// leave the `int` range and come back. Terms of one sign cannot bring
/// it back (#2423).
#[test]
fn a_sum_is_a_site_when_its_terms_have_one_sign() {
    let none: Vec<Vec<String>> = vec![vec![]];
    for term in ["sq(i)", "i", "weight(i)", "abs(i - 50)", "7", "i & 3", "i / 2 + i % 3", "0 - i", "0 - sq(i)", "0", "abs(k)", "sq(k) * i"] {
        assert_eq!(fold_guards("int n, int k", "5", "0..n", "+", term), none, "`{term}`");
    }
    // The sign of a capture is fixed for a run of the loop: whichever it
    // is, every term has it.
    for term in ["k", "i * k", "weight(k)", "i ^ k", "0 - k * sq(i)"] {
        assert_eq!(fold_guards("int n, int k", "5", "0..n", "+", term), none, "`{term}`");
    }
    for term in ["step(i)", "i - 50", "sq(i) - 1", "0 - weight(i - 3)", "step(i) * k", "i - k * k"] {
        assert!(fold_sites("int n, int k", "5", "0..n", "+", term).is_empty(), "`{term}`");
    }
}

/// Where no value of a capture or bound signs the fold whatever it is,
/// a test of it does: the fork-join takes the chunked path only when it
/// holds, and runs the loop on its own thread otherwise.
#[test]
fn a_run_time_test_signs_a_fold_a_capture_decides() {
    let guards = |params: &str, range: &str, op: &str, term: &str| fold_guards(params, "1", range, op, term);
    // The index from a start read at run time.
    assert_eq!(guards("int a, int n", "a..n", "+", "i"), [["a >= 0"]]);
    assert_eq!(guards("int a, int n", "a..n", "+", "weight(i)"), [["a >= 0"]]);
    // A product of a capture: `0` and `-1` are not allowed.
    assert_eq!(guards("int n, int k", "0..n", "*", "k"), [["k >= 1"]]);
    assert_eq!(guards("int n, int k", "0..n", "*", "k + 1"), [["k >= 0"]]);
    assert_eq!(guards("int n, int k", "0..n", "*", "i + k"), [["k >= 1"]]);
    assert_eq!(guards("int n, int k", "0..n", "*", "0 - k"), [["k >= 2"]]);
    // `abs(k) - 1` is `-1` at `0` and `0` at `1` and `-1`.
    assert_eq!(guards("int n, int k", "0..n", "*", "abs(k) - 1"), [["k >= 2"]]);
    // Two captures: each tested.
    assert_eq!(guards("int n, int j, int k", "0..n", "+", "j * k + i"), [["j >= 0", "k >= 0"]]);
    // One sign for some values only.
    assert_eq!(guards("int n, int k", "0..n", "+", "i - k"), [["k <= 0"]]);
    assert_eq!(guards("int n, int k", "0..n", "+", "k - sq(i)"), [["k <= 0"]]);
    // A term of both signs for every value of the captures.
    assert!(guards("int n, int k", "0..n", "+", "step(i) * k").is_empty());
    assert!(guards("int n, int k", "0..n", "*", "i * k").is_empty());
}

/// The index of a range that may start below zero has no sign.
#[test]
fn the_index_has_the_sign_of_its_range() {
    // A start read at run time is tested.
    assert_eq!(fold_guards("int a, int n", "0", "a..n", "+", "i"), [["a >= 0"]]);
    assert_eq!(fold_guards("int a, int n", "0", "a..n", "+", "weight(i)"), [["a >= 0"]]);
    assert_eq!(fold_guards("int a, int n", "0", "a..n", "+", "sq(i)"), vec![Vec::<String>::new()]);
    assert_eq!(fold_sites("int n", "0", "3..n", "+", "i - 3").len(), 1);
    assert!(fold_sites("int n", "0", "3..n", "+", "i - 4").is_empty());
    // A `while` reads its index after the step too.
    let down = |end: &str| {
        sites_of(&format!(
            r#"
fn run() -> int {{
    let acc = 0;
    let i = 0 - 100;
    while i < {end} {{
        acc = acc + i;
        i = i + 1;
    }}
    return acc;
}}
fn main() {{ return; }}
"#
        ))
    };
    assert_eq!(down("0").len(), 1);
    assert!(down("1").is_empty());
}

/// A product stays out of range once it is out only when no term is `0`
/// or `-1`: `2^63 * -1` is `MIN`, which is in range.
#[test]
fn a_product_is_a_site_when_no_term_is_zero_or_minus_one() {
    for term in ["2", "i + 1", "sq(i) + 1", "0 - 2", "0 - i - 2", "1"] {
        assert_eq!(fold_sites("int n, int k", "1", "0..n", "*", term).len(), 1, "`{term}`");
    }
    for term in ["i", "0", "0 - 1", "0 - i - 1", "weight(i)", "i - 1", "sq(i)"] {
        assert!(fold_sites("int n, int k", "1", "0..n", "*", term).is_empty(), "`{term}`");
    }
    // A capture is tested to be `>= 1` at run time.
    assert_eq!(fold_guards("int n, int k", "1", "0..n", "*", "k"), [["k >= 1"]]);
}

/// `^` never traps.
#[test]
fn an_xor_fold_is_a_site_whatever_its_terms() {
    for term in ["step(i)", "i - 50", "k", "0 - 1"] {
        assert_eq!(fold_sites("int n, int k", "5", "0..n", "^", term).len(), 1, "`{term}`");
    }
}

/// Every arm of an `if` folds one way, and a condition narrows what its
/// arm reads.
#[test]
fn the_arms_of_an_if_agree_on_the_sign() {
    let run = |body: &str| {
        sites_of(&format!(
            r#"
fn run(int n, int k) -> int {{
    let acc = 0;
    for i in 0..n {{
        {body}
    }}
    return acc;
}}
fn main() {{ return; }}
"#
        ))
    };
    assert_eq!(run("if i % 2 == 0 { acc = acc + i; } else { acc = acc + 1; }").len(), 1);
    assert!(run("if i % 2 == 0 { acc = acc + i; } else { acc = acc - 0 + (0 - 1); }").is_empty());
    assert!(run("if i % 2 == 0 { acc = acc + i; } else { acc = acc + (0 - 1); }").is_empty());
    // `k` is not negative where the first arm reads it, and negative in
    // the second.
    assert_eq!(run("if k >= 0 { acc = acc + k; }").len(), 1);
    // Both arms fold `k`, whose sign is the same in every iteration.
    assert_eq!(run("if k >= 0 { acc = acc + k; } else { acc = acc + k; }").len(), 1);
    // `k` in one arm and `-k` in the other have opposite signs.
    assert!(run("if i % 2 == 0 { acc = acc + k; } else { acc = acc + (0 - k); }").is_empty());
    assert_eq!(run("if k >= 0 { acc = acc + k; } else { acc = acc + (0 - k); }").len(), 1);
    assert_eq!(run("let d = i - 5; if d > 0 { acc = acc + d; }").len(), 1);
    assert!(run("let d = i - 5; acc = acc + d;").is_empty());
}

/// What the function establishes ahead of the loop holds in it: a guard
/// that returns, and a `let` nothing writes again.
#[test]
fn a_guard_ahead_of_the_loop_signs_a_capture() {
    // A product of `k`: a site with no run-time test only where `k >= 1`
    // (or `k <= -2`) is known.
    let run = |ahead: &str, term: &str| -> Vec<Vec<String>> {
        sites_of(&format!(
            r#"
fn run(int n, int k) -> int {{
    {ahead}
    let acc = 1;
    for i in 0..n {{
        acc = acc * ({term});
    }}
    return acc;
}}
fn main() {{ return; }}
"#
        ))
        .iter()
        .map(|s| s.guard.iter().map(ToString::to_string).collect())
        .collect()
    };
    let none: Vec<Vec<String>> = vec![vec![]];
    assert_eq!(run("", "k"), [["k >= 1"]]);
    assert_eq!(run("if k < 1 { return 0; }", "k"), none);
    assert_eq!(run("if k < 1 { panic \"k\"; }", "k"), none);
    assert_eq!(run("let w = (k & 7) + 1;", "w"), none);
    assert_eq!(run("let w = (k & 7) - 1;", "w"), [["w >= 1"]]);
    // The guard is about a `k` that is written afterwards.
    assert_eq!(run("if k < 1 { return 0; } k = k - 9;", "k"), [["k >= 1"]]);
    // A lambda may write it at any call: no site.
    assert!(run("if k < 1 { return 0; } let f = fn () { k = 0; };", "k").is_empty());
}

/// A `for` runs its checks ahead of each iteration, so a clause that
/// passed holds in the body.
#[test]
fn a_loop_invariant_signs_a_capture() {
    let sites = checked_sites_in(
        "int n, int k",
        r#"
    let acc = 0;
    for i in 0..n
        invariant k > 0
    {
        acc = acc + i * k;
    }
    return acc;
"#,
    );
    assert_eq!(sites.len(), 1, "{sites:?}");
}

fn checked_sites_in(params: &str, body: &str) -> Vec<LoopParSite> {
    crate::hir::set_contract_level(crate::hir::ContractLevel::All);
    sites_of(&format!("fn run({params}) -> int {{\n{body}\n}}\nfn main() {{ return; }}\n"))
}

/// The fork-join works on `[begin, end)`: an inclusive end read at run
/// time must be one it can add one to, where the sequential loop runs
/// through `MAX`.
#[test]
fn an_inclusive_end_that_may_be_max_keeps_the_loop_sequential() {
    assert!(fold_sites("int n", "0", "0..=n", "^", "i").is_empty());
    assert_eq!(fold_sites("int n", "0", "0..n", "^", "i").len(), 1);
    let bounded = sites_of(
        r#"
fn run(int n) -> int {
    if n > 1000000 { return 0; }
    let acc = 0;
    for i in 0..=n {
        acc = acc ^ i;
    }
    let other = 0;
    let j = 0;
    while j <= n {
        other = other ^ j;
        j = j + 1;
    }
    return acc + other;
}
fn open(int n) -> int {
    let acc = 0;
    let j = 0;
    while j <= n {
        acc = acc ^ j;
        j = j + 1;
    }
    return acc;
}
fn main() { return; }
"#,
    );
    assert_eq!(bounded.len(), 2, "{bounded:?}");
}

/// `for _ in` counts like any other range: its index has a local, which
/// the body cannot name.
#[test]
fn a_loop_with_no_name_for_its_index_is_a_site() {
    assert!(reach().contains(&105), "{:?}", reach());
    // The builder calls that local `item`; a capture of the same name is
    // not it.
    let sites = sites_of(
        r#"
fn run(int n, int item) -> int {
    let acc = 0;
    for _ in 0..n { acc = acc ^ item; }
    return acc;
}
fn main() { return; }
"#,
    );
    assert!(sites.is_empty(), "{sites:?}");
}

/// An `if let` is a `match`, and a `while let` a loop that is not
/// counted: a counted loop inside either is a site.
#[test]
fn a_loop_under_if_let_or_while_let_is_a_site() {
    let ends = reach();
    assert!(ends.contains(&103) && ends.contains(&106), "{ends:?}");
}

/// A trait's method is a body like any other: an instance's, and a
/// default one.
#[test]
fn a_loop_in_a_trait_method_is_a_site() {
    let ends = reach();
    assert!(ends.contains(&101) && ends.contains(&102), "{ends:?}");
    assert_eq!(ends, [101, 102, 103, 104, 105, 106]);
}
