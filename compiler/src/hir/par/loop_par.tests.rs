//! Which counted loops are fork sites.

use super::*;
use crate::hir::build_module;
use crate::typechecking::bare_cases::BareCases;
use crate::typechecking::infer::Checker;
use crate::typechecking::purity::analyze_pure_fns;

fn sites_of(src: &str) -> Vec<LoopParSite> {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let pure = analyze_pure_fns(&ast, BareCases::none());
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    let mut sites: Vec<LoopParSite> = module.loop_par_sites(&checker, &pure).into_values().collect();
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

#[test]
fn admits_a_loop_under_its_contract_checks() {
    let site = one_site(
        r#"
fn sq(int i) -> int { return i * i; }
fn run(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n invariant i >= 0 {
        acc = acc + sq(i);
        i = i + 1;
    }
    return acc;
}
fn main() { return; }
"#,
    );
    assert_eq!((site.index.as_str(), site.acc.as_str()), ("i", "acc"));
    let site = one_site(&program(
        r#"
    let acc = 0;
    for x in 0..100 invariant acc >= 0 {
        acc = acc + sq(x);
    }
"#,
    ));
    assert!(site.implicit_step);
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

/// The reach of the AST walk this analysis replaced, kept as it was.
#[test]
fn leaves_trait_methods_if_let_and_const_bounds() {
    let sites = sites_of(
        r#"
fn sq(int i) -> int { return i * i; }
class C { pub k: int, }
trait Tot<A> {
    fn tot(A self) -> int {}
    fn twice(A self) -> int {
        let acc = 0;
        for x in 0..100 { acc = acc + sq(x); }
        return acc;
    }
}
impl Tot for C {
    fn tot(C self) -> int {
        let acc = 0;
        for x in 0..100 { acc = acc + sq(x); }
        return acc;
    }
}
fn under_if_let(Option<int> o) -> int {
    let acc = 0;
    if let Some(v) = o { for x in 0..100 { acc = acc + sq(x); } }
    return acc;
}
fn const_bound() -> int {
    const N = 100;
    let acc = 0;
    for x in 0..N { acc = acc + sq(x); }
    return acc;
}
fn unnamed_index() -> int {
    let acc = 0;
    for _ in 0..100 { acc = acc + sq(3); }
    return acc;
}
fn main() { return; }
"#,
    );
    assert!(sites.is_empty(), "{sites:?}");
}
