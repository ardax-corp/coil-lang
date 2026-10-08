use super::*;
use crate::hir::{HirId, LocalId};

fn variant(v: &str, fields: HirPatFields) -> HirPat {
    HirPat::Variant {
        enum_name: "E".into(),
        variant: v.into(),
        tag: None,
        fields,
    }
}

fn arm(pat: HirPat, body: u32) -> HirArm {
    HirArm { pat, body: HirId(body) }
}

fn some(p: HirPat) -> HirPatFields {
    HirPatFields::Tuple(vec![p])
}

#[test]
fn groups_rows_by_outer_variant_in_first_seen_order() {
    let arms = vec![
        arm(variant("Ok", some(HirPat::Int(1))), 0),
        arm(variant("Err", some(HirPat::Wild)), 1),
        arm(variant("Ok", some(HirPat::Bind(LocalId(0)))), 2),
    ];
    let tree = outer_groups(&arms).expect("two Ok rows group");
    let names: Vec<_> = tree.groups.iter().map(|g| (g.variant, g.rows.len())).collect();
    assert_eq!(names, vec![("Ok", 2), ("Err", 1)]);
    assert!(tree.catch_all.is_none());
}

#[test]
fn drops_rows_after_an_irrefutable_row_and_keeps_the_catch_all() {
    let arms = vec![
        arm(variant("A", some(HirPat::Int(0))), 0),
        arm(variant("A", some(HirPat::Wild)), 1),
        arm(variant("A", some(HirPat::Int(2))), 2),
        arm(HirPat::Wild, 3),
    ];
    let tree = outer_groups(&arms).expect("grouped");
    assert_eq!(tree.groups[0].rows.len(), 2);
    assert_eq!(tree.catch_all.map(|a| a.body), Some(HirId(3)));
}

#[test]
fn declines_when_no_variant_repeats_or_a_row_is_not_a_variant() {
    let flat = vec![
        arm(variant("A", some(HirPat::Int(0))), 0),
        arm(variant("B", some(HirPat::Wild)), 1),
    ];
    assert!(outer_groups(&flat).is_none());
    let tuple = vec![
        arm(HirPat::Tuple(vec![HirPat::Int(0)]), 0),
        arm(variant("A", some(HirPat::Int(0))), 1),
        arm(variant("A", some(HirPat::Wild)), 2),
    ];
    assert!(outer_groups(&tuple).is_none());
}

fn lit(p: &HirPat) -> Option<i64> {
    match p {
        HirPat::Int(n) => Some(*n),
        _ => None,
    }
}

#[test]
fn int_search_sorts_cases_and_keeps_the_first_arm_per_literal() {
    let mut arms: Vec<HirArm> = [9, 3, 7, 1, 3, 5, 8, 2, 6]
        .iter()
        .enumerate()
        .map(|(i, &n)| arm(HirPat::Int(n), i as u32))
        .collect();
    arms.push(arm(HirPat::Wild, 9));
    let plan = int_search(&arms, lit).expect("eight distinct literals");
    assert_eq!(
        plan.cases,
        vec![(1, 3), (2, 7), (3, 1), (5, 5), (6, 8), (7, 2), (8, 6), (9, 0)]
    );
    assert_eq!(plan.default, 9);
}

#[test]
fn int_search_declines_short_or_mixed_matches() {
    let short: Vec<HirArm> = (0..4)
        .map(|n| arm(HirPat::Int(n), n as u32))
        .chain([arm(HirPat::Wild, 4)])
        .collect();
    assert!(int_search(&short, lit).is_none());
    let mut mixed: Vec<HirArm> = (0..9).map(|n| arm(HirPat::Int(n), n as u32)).collect();
    mixed.insert(3, arm(HirPat::Wild, 99));
    assert!(int_search(&mixed, lit).is_none());
}
