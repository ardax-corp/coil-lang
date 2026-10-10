use super::*;

fn sample() -> Proof {
    let mut p = Proof { complete: true, ..Proof::default() };
    p.checks.insert(("clamp".into(), 10, 30, "ensures result >= lo".into()));
    p.bounds.insert(("last".into(), 40, 52, String::new()));
    p
}

#[test]
fn a_proof_reads_back_for_its_own_source() {
    let text = sample().render("fn f() {}");
    assert!(text.starts_with("coil-proof 1\ncompiler "), "{text}");
    assert_eq!(Proof::parse(&text, "fn f() {}"), Some(sample()));
}

#[test]
fn an_edited_source_makes_the_proof_stale() {
    let text = sample().render("fn f() {}");
    assert_eq!(Proof::parse(&text, "fn f() { }"), None);
}

#[test]
fn a_damaged_proof_is_ignored() {
    let text = sample().render("x").replace("bounds last\t40", "bounds last\tforty");
    assert_eq!(Proof::parse(&text, "x"), None);
    assert_eq!(Proof::parse("not a proof", "x"), None);
}

#[test]
fn the_proof_sits_next_to_the_file() {
    assert_eq!(path_for(Path::new("src/main.hy")), PathBuf::from("src/main.hy.proof"));
}

const SRC: &str = "fn clamp(int x) -> int ensures result >= 0 { if x < 0 { return 0; } return x; }
fn last(Vec<int> v) -> int requires len(v) > 0 { return v[len(v) - 1]; }";

/// The entry module's HIR, built with `proof` applied.
fn build(proof: impl FnOnce(&[FnCheck]) -> Proof) -> crate::hir::HirModule {
    let owned = Box::leak(SRC.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = crate::typechecking::infer::Checker::new();
    let _ = checker.check_program(&ast);
    crate::hir::set_contract_level(crate::hir::ContractLevel::All);
    let sidecar = checker.typed_sidecar();
    let checks = super::super::encode::verify_module(&crate::hir::build_module(&checker, &sidecar, "", &ast));
    let _scope = ProofScope::set(Some(proof(&checks)));
    crate::hir::build_module(&checker, &sidecar, "", &ast)
}

fn texts(module: &crate::hir::HirModule, name: &str) -> Vec<String> {
    let body = module.bodies.iter().find(|b| b.name == name).unwrap();
    body.exprs
        .iter()
        .filter_map(|e| match &e.kind {
            crate::hir::HirKind::Lit(crate::hir::Lit::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn in_bounds(module: &crate::hir::HirModule, name: &str) -> bool {
    let body = module.bodies.iter().find(|b| b.name == name).unwrap();
    body.exprs
        .iter()
        .any(|e| matches!(e.kind, crate::hir::HirKind::Index { .. }) && e.flags.contains(crate::hir::HirFlags::IN_BOUNDS))
}

#[test]
fn a_build_drops_the_proved_checks_and_keeps_requires() {
    let module = build(|checks| Proof::from_outcomes(checks, |_, _| Outcome::Proved));
    assert!(texts(&module, "clamp").iter().all(|t| !t.contains("ensures")), "{:?}", texts(&module, "clamp"));
    assert!(texts(&module, "last").iter().any(|t| t.contains("requires len(v) > 0")));
    assert!(in_bounds(&module, "last"));
}

#[test]
fn an_incomplete_proof_keeps_every_bounds_check() {
    let module = build(|checks| {
        Proof::from_outcomes(checks, |i, _| if checks[i].name == "clamp" { Outcome::Unknown } else { Outcome::Proved })
    });
    assert!(texts(&module, "clamp").iter().any(|t| t.contains("ensures")));
    assert!(!in_bounds(&module, "last"));
}
