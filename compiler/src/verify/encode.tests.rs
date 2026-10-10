//! The goals and queries the encoder builds, without a solver
//! (`coil-verify/tests` runs them through z3).

use super::*;
use crate::typechecking::infer::Checker;

fn checks(src: &str) -> Vec<FnCheck> {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    assert!(
        checker.messages().iter().all(|m| *m.kind() != reporting::MessageKind::ERROR),
        "{:?}",
        checker.messages()
    );
    crate::hir::set_contract_level(crate::hir::ContractLevel::All);
    let sidecar = checker.typed_sidecar();
    let module = crate::hir::build_module(&checker, &sidecar, "", &ast);
    verify_module(&module)
}

fn named<'a>(checks: &'a [FnCheck], name: &str) -> &'a FnCheck {
    checks.iter().find(|c| c.name == name).unwrap_or_else(|| panic!("no checks for `{name}`"))
}

#[test]
fn every_return_checks_the_ensures_and_the_requires_is_assumed() {
    let all = checks(
        "fn clamp(int x, int lo, int hi) -> int
            requires lo <= hi
            ensures result >= lo && result <= hi
        {
            if x < lo { return lo; }
            if x > hi { return hi; }
            return x;
        }",
    );
    let clamp = named(&all, "clamp");
    let [goal] = clamp.goals.as_slice() else { panic!("{:#?}", clamp.goals) };
    assert_eq!(goal.keyword, "ensures");
    assert_eq!(goal.clause, "ensures result >= lo && result <= hi");
    assert_eq!(goal.queries.len(), 3, "one per return");
    assert!(goal.queries.iter().all(|q| q.exact));
    let q = &goal.queries[0].smt;
    assert!(q.contains("(declare-const p_x (_ BitVec 64))"), "{q}");
    assert!(q.contains("(get-value (p_x p_lo p_hi))"), "{q}");
    let names: Vec<_> = clamp.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["x", "lo", "hi"]);
}

#[test]
fn a_call_must_establish_the_callee_requires_and_assumes_its_ensures() {
    let all = checks(
        "fn half(int x) -> int requires x >= 0 ensures result >= 0 { return x / 2; }
         fn quarter(int x) -> int ensures result >= 0 { return half(half(x)); }",
    );
    let quarter = named(&all, "quarter");
    let calls: Vec<_> = quarter.goals.iter().filter(|g| g.callee.as_deref() == Some("half")).collect();
    assert_eq!(calls.len(), 2, "{:#?}", quarter.goals);
    assert_eq!(calls[0].clause, "requires x >= 0");
    let ensures = quarter.goals.iter().find(|g| g.keyword == "ensures").unwrap();
    // The result is the callee's, so a model may not be a real input.
    assert!(ensures.queries.iter().all(|q| !q.exact));
}

#[test]
fn a_loop_invariant_is_checked_on_entry_and_on_the_way_back() {
    let all = checks(
        "fn sum_to(int n) -> int
            requires n >= 0
            ensures result >= 0
        {
            let s = 0;
            let i = 0;
            while i < n invariant s >= 0 && i >= 0 && i <= n {
                i = i + 1;
                s = s + i;
            }
            return s;
        }",
    );
    let f = named(&all, "sum_to");
    let inv = f.goals.iter().find(|g| g.keyword == "invariant").unwrap();
    assert_eq!(inv.queries.len(), 2, "entry and back edge");
    assert!(inv.queries[0].exact, "entry sees the real state");
    assert!(!inv.queries[1].exact, "the back edge starts from a cut state");
}

#[test]
fn decreases_is_a_goal_of_its_own_and_generic_functions_are_skipped() {
    let all = checks(
        "fn down(int n) -> int requires n >= 0 {
            let i = n;
            while i > 0 decreases i { i = i - 1; }
            return i;
        }
        fn id<T>(T x) -> T ensures true { return x; }",
    );
    let down = named(&all, "down");
    assert!(down.goals.iter().any(|g| g.keyword == "decreases"), "{:#?}", down.goals);
    assert!(all.iter().all(|c| c.name != "id"));
}

#[test]
fn trapping_overflow_assumes_the_result_fits() {
    let all = checks("fn inc(int x) -> int ensures result > x { return x + 1; }");
    let q = &named(&all, "inc").goals[0].queries[0].smt;
    assert!(q.contains("((_ sign_extend 1) p_x)"), "{q}");
}

#[test]
fn a_vec_parameter_is_a_length_and_its_items() {
    let all = checks(
        "fn first(Vec<int> v) -> int requires len(v) > 0 { return v[0]; }
         fn grow(Vec<int> v) -> int { v.push(1); return first(v); }",
    );
    let grow = named(&all, "grow");
    let [goal] = grow.goals.as_slice() else { panic!("{:#?}", grow.goals) };
    assert_eq!(goal.callee.as_deref(), Some("first"));
    assert!(matches!(grow.params[0].shape, ParamShape::Seq { .. }));
    assert!(goal.queries[0].exact, "push is modelled exactly");
    assert!(goal.queries[0].smt.contains("(declare-const p_v_len (_ BitVec 64))"));
}
