use super::*;

fn patches(source: &str, operators: &[Operator]) -> Vec<(u32, String)> {
    enumerate(source, operators)
        .expect("parses")
        .iter()
        .map(|s| (s.line, s.apply(source)))
        .collect()
}

#[test]
fn binary_operators_swap_at_the_operator_token() {
    let src = "fn f(int a, int b) -> bool {\n    return (a + 1) * b <= 10 && a != b;\n}\n";
    let got: Vec<String> = patches(src, &Operator::ALL)
        .into_iter()
        .map(|(_, s)| s.lines().nth(1).unwrap().trim().to_string())
        .collect();
    assert_eq!(
        got,
        [
            "return (a - 1) * b <= 10 && a != b;",
            "return (a + 0) * b <= 10 && a != b;",
            "return (a + 1) / b <= 10 && a != b;",
            "return (a + 1) * b < 10 && a != b;",
            "return (a + 1) * b <= 11 && a != b;",
            "return (a + 1) * b <= 10 || a != b;",
            "return (a + 1) * b <= 10 && a == b;",
        ]
    );
}

#[test]
fn conditions_and_literals() {
    let src = "fn g(int n) -> bool {\n    let ok = true;\n    while n > 0 {\n        n = n - 2;\n    }\n    if ok {\n        return false;\n    }\n    return ok;\n}\n";
    let ops = [Operator::Cond, Operator::Bool];
    let got: Vec<(u32, String)> = patches(src, &ops)
        .into_iter()
        .map(|(line, s)| {
            (
                line,
                s.lines().nth(line as usize - 1).unwrap().trim().to_string(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (2, "let ok = false;".to_string()),
            (3, "while !(n > 0) {".to_string()),
            (6, "if !(ok) {".to_string()),
            (7, "return true;".to_string()),
        ]
    );
}

#[test]
fn operator_filter_and_line_numbers() {
    let src = "fn h(int a) -> int {\n    return a\n        - 3;\n}\n";
    let sites = enumerate(src, &[Operator::Arith]).unwrap();
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].line, 3);
    assert_eq!(sites[0].scope_line, 1);
    assert_eq!(sites[0].describe(src), "`-` → `+`");
}

#[test]
fn tests_and_no_mutate_are_skipped() {
    let src = "\
fn keep(int a) -> int { return a + 1; }
fn quiet(int a) -> int { return a + 1; } // coil:no-mutate
// coil:no-mutate
fn skip(int a) -> int {
    return a + 1;
}
#[test]
fn t() -> Result<(), string> { assert(keep(1) == 2)?; }
test(\"x\") { assert(1 + 1 == 2)?; }
";
    let sites = enumerate(src, &Operator::ALL).unwrap();
    assert!(sites.iter().all(|s| s.line == 1), "{sites:?}");
    assert_eq!(sites.len(), 2);
}

#[test]
fn spans_are_bytes_past_multibyte_text() {
    let src = "fn s() -> bool { let t = \"é→\"; return 1 < 2; }\n";
    let sites = enumerate(src, &[Operator::Boundary]).unwrap();
    assert_eq!(sites.len(), 1);
    assert!(sites[0].apply(src).contains("return 1 <= 2;"));
}

#[test]
fn sites_outside_fns_are_their_own_scope_and_no_ops_are_dropped() {
    let src = "fn f() -> int {\n    return 1;\n}\nstatic let n: int = 2 + 9223372036854775807;\n";
    let sites = enumerate(src, &Operator::ALL).unwrap();
    let described: Vec<(u32, u32, String)> = sites
        .iter()
        .map(|s| (s.line, s.scope_line, s.describe(src)))
        .collect();
    assert_eq!(
        described,
        [
            (2, 1, "`1` → `0`".to_string()),
            (4, 4, "`2` → `3`".to_string()),
            (4, 4, "`+` → `-`".to_string()),
        ]
    );
}

#[test]
fn operator_names_round_trip() {
    for op in Operator::ALL {
        assert_eq!(Operator::parse(op.name()), Some(op));
    }
    assert_eq!(Operator::parse("nope"), None);
}

#[test]
fn contract_clauses_are_not_mutated() {
    // The clauses are the oracle a mutant must break, not code under test.
    let src = "fn f(int x) -> int\n    requires x >= 0\n    ensures result > x\n{\n    return x + 1;\n}\n";
    let lines: Vec<u32> = patches(src, &Operator::ALL).into_iter().map(|(line, _)| line).collect();
    assert_eq!(lines, [5, 5]);
}
