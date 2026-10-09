use super::*;
use ast::{ContractKind, Expression};

fn contracts_of(src: &str) -> Vec<(ContractKind, String, Option<String>)> {
    let out = Pratt::default().parse(src).expect("parse failed");
    let Expression::Program(items) = *out.1 else {
        panic!("expected program")
    };
    let Expression::Function { contracts, .. } = items[0].1.as_ref() else {
        panic!("expected a function, got {:?}", items[0].1)
    };
    contracts
        .iter()
        .map(|c| (c.kind, c.text.to_string(), c.message.map(str::to_string)))
        .collect()
}

#[test]
fn requires_and_ensures_parse_in_order() {
    let got = contracts_of(
        "fn isqrt(int n) -> int\n    requires n >= 0\n    ensures result * result <= n, \"too big\"\n    ensures (result + 1) * (result + 1) > n\n{\n    return 0;\n}\n",
    );
    assert_eq!(
        got,
        vec![
            (ContractKind::Requires, "n >= 0".into(), None),
            (ContractKind::Ensures, "result * result <= n".into(), Some("too big".into())),
            (ContractKind::Ensures, "(result + 1) * (result + 1) > n".into(), None),
        ]
    );
}

#[test]
fn contracts_follow_where_and_uses() {
    let got = contracts_of("fn f<T>(T x) -> T where Num<T> uses {read} requires x > 0 { return x; }");
    assert_eq!(got, vec![(ContractKind::Requires, "x > 0".into(), None)]);
    let got = contracts_of("fn f(Vec<int> xs) ensures match result { Option::Some(i) => xs[i] > 0, Option::None => true } { }");
    assert_eq!(got.len(), 1);
    assert!(got[0].1.starts_with("match result {"), "{got:?}");
    assert!(contracts_of("fn f() { }").is_empty());
}

#[test]
fn contract_words_stay_identifiers() {
    let src = "fn requires(int ensures) -> int {\n    let result = ensures;\n    return result;\n}\n";
    assert_eq!(crate::format_source(src).expect("format"), src);
    assert!(contracts_of(src).is_empty());
}

#[test]
fn format_puts_each_contract_on_its_own_line() {
    let src = "fn isqrt(int n) -> int requires n >= 0 ensures result * result <= n, \"too big\" { return 0; }\n";
    let want = "fn isqrt(int n) -> int\n    requires n >= 0\n    ensures result * result <= n, \"too big\"\n{\n    return 0;\n}\n";
    assert_eq!(crate::format_source(src).expect("format"), want);
    assert_eq!(crate::format_source(want).expect("format"), want);
    let methods = "impl Ring {\n    fn push(int x)\n        requires x > 0\n    {\n        return;\n    }\n}\n";
    assert_eq!(crate::format_source(methods).expect("format"), methods);
}
