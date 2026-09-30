use super::*;
use ast::{Expression, QuoteKind, QuotePart};

fn parse_program(src: &str) -> Vec<Output<'_>> {
    let out = Pratt::default().parse(src).expect("parse failed");
    match *out.1 {
        Expression::Program(items) => items,
        other => panic!("expected program, got {other:?}"),
    }
}

fn find_quote<'a>(e: &'a Output<'a>) -> Option<&'a Expression<'a>> {
    if matches!(e.1.as_ref(), Expression::Quote { .. }) {
        return Some(e.1.as_ref());
    }
    let mut found = None;
    e.1.for_each_child(&mut |c| {
        if found.is_none() {
            found = find_quote(c);
        }
    });
    found
}

#[test]
fn derive_decl_parses_signature_and_helpers() {
    let items = parse_program(
        "/// Doc.\nderive ToJson(TypeDecl t) -> Items attrs(json, skip) { return t.name; }",
    );
    assert_eq!(items.len(), 1);
    let Expression::DeriveDecl {
        docs,
        name,
        returns,
        helpers,
        ..
    } = items[0].1.as_ref()
    else {
        panic!("expected DeriveDecl, got {:?}", items[0].1);
    };
    assert_eq!(*name, "ToJson");
    assert_eq!(docs, &vec!["Doc."]);
    assert!(returns.is_some());
    assert_eq!(helpers, &vec!["json", "skip"]);
}

#[test]
fn derive_stays_an_identifier() {
    let items = parse_program("fn derive(int x) -> int { return x; }\nfn main() { derive(1); }");
    assert_eq!(items.len(), 2);
    assert!(matches!(items[0].1.as_ref(), Expression::Function { name: "derive", .. }));
}

#[test]
fn quote_splits_text_and_holes() {
    let items = parse_program(
        "fn f(Ident n) -> Items { return quote items { impl Show for ${n} { fn a() { $(xs),* } } }; }",
    );
    let q = find_quote(&items[0]).expect("quote");
    let Expression::Quote { kind, parts } = q else { unreachable!() };
    assert_eq!(*kind, QuoteKind::Items);
    let splices = parts.iter().filter(|p| matches!(p, QuotePart::Splice(_))).count();
    assert_eq!(splices, 1);
    let seps: Vec<&str> = parts
        .iter()
        .filter_map(|p| match p {
            QuotePart::Repeat { sep, .. } => Some(*sep),
            _ => None,
        })
        .collect();
    assert_eq!(seps, vec![","]);
    let text: String = parts
        .iter()
        .map(|p| match p {
            QuotePart::Lit(t) => t.to_string(),
            QuotePart::Splice(_) => "#".into(),
            QuotePart::Repeat { .. } => "@".into(),
        })
        .collect();
    assert_eq!(text, " impl Show for # { fn a() { @ } } ");
}

#[test]
fn quote_keeps_holes_inside_strings_as_text() {
    let items = parse_program(r#"fn f() -> Frag { return quote expr { "${x}" + "}" }; }"#);
    let Expression::Quote { parts, .. } = find_quote(&items[0]).expect("quote") else {
        unreachable!()
    };
    assert!(parts.iter().all(|p| matches!(p, QuotePart::Lit(_))));
}

#[test]
fn quote_call_stays_a_call() {
    let items = parse_program("fn quote(string s) -> string { return s; }\nfn g() -> string { return quote(\"a\"); }");
    assert_eq!(items.len(), 2);
    assert!(find_quote(&items[1]).is_none());
}

#[test]
fn field_and_variant_attrs_parse() {
    let items = parse_program(
        "class C {\n    #[json(rename = \"p\")]\n    pub port: int,\n}\nenum E {\n    #[json(skip)]\n    A,\n    B,\n}",
    );
    let Expression::Class { fields, .. } = items[0].1.as_ref() else { panic!() };
    let Expression::Field { attrs, .. } = fields[0].1.as_ref() else { panic!() };
    assert_eq!(attrs.len(), 1);
    assert_eq!(attrs[0].name, "json");
    let Expression::EnumDecl { variants, .. } = items[1].1.as_ref() else { panic!() };
    let Expression::EnumVariant { attrs, .. } = variants[0].1.as_ref() else { panic!() };
    assert_eq!(attrs[0].name, "json");
}

#[test]
fn format_round_trips_derive_and_quote() {
    let src = "derive D(TypeDecl t) -> Items attrs(h) {\n    return quote items { impl X for ${t.name} {} };\n}\n";
    let out = crate::format_source(src).expect("format");
    assert_eq!(out, src);
}

#[test]
fn format_keeps_method_attributes_before_pub() {
    let src = "impl C {\n    #[twice]\n    pub fn get() -> int {\n        return 1;\n    }\n}\n";
    assert_eq!(crate::format_source(src).expect("format"), src);
}

/// Synthetic trees (built-in derives) have no `Group`: the formatter must
/// add the parentheses precedence needs.
#[test]
fn format_parenthesizes_by_precedence() {
    use ast::Output;
    let sp = SimpleSpan::from(0..1);
    let id = |n: &'static str| -> Output<'static> { (sp, Box::new(Expression::Identifier(n))) };
    let node = |e: Expression<'static>| -> Output<'static> { (sp, Box::new(e)) };
    let not_eq = node(Expression::LogicalNot(node(Expression::Eq(id("a"), id("b")))));
    let or_in_and = node(Expression::And(id("p"), node(Expression::Or(id("q"), id("r")))));
    let sub_right = node(Expression::Sub(id("a"), node(Expression::Sub(id("b"), id("c")))));
    let mul_of_add = node(Expression::Mul(node(Expression::Add(id("a"), id("b"))), id("c")));
    let out = |e: &Output<'_>| crate::format_program(e.1.as_ref());
    assert_eq!(out(&not_eq).trim(), "!(a == b)");
    assert_eq!(out(&or_in_and).trim(), "p && (q || r)");
    assert_eq!(out(&sub_right).trim(), "a - (b - c)");
    assert_eq!(out(&mul_of_add).trim(), "(a + b) * c");
}

fn find_macro_calls<'a>(e: &'a Output<'a>, out: &mut Vec<(&'a str, usize)>) {
    if let Expression::MacroCall { name, args } = e.1.as_ref() {
        out.push((name, args.len()));
    }
    e.1.for_each_child(&mut |c| find_macro_calls(c, out));
}

#[test]
fn fn_macro_decl_parses_and_macro_stays_a_module_name() {
    let items = parse_program(
        "use macro::{Expr, Code};\n/// Doc.\nmacro twice(Expr e, Vec<Expr> rest) -> Code { return quote expr { ${e} * 2 }; }",
    );
    assert_eq!(items.len(), 2);
    let Expression::FnMacroDecl { docs, name, returns, .. } = items[1].1.as_ref() else {
        panic!("expected FnMacroDecl, got {:?}", items[1].1);
    };
    assert_eq!(*name, "twice");
    assert_eq!(docs, &vec!["Doc."]);
    assert!(returns.is_some());
}

#[test]
fn macro_call_parses_in_expression_statement_and_item_position() {
    let items = parse_program(
        "consts!(A, B);\nfn main() {\n    let x = twice!(1 + 2) + f(3);\n    log!();\n    if a != b && !c { y!(v.w, g(1)); }\n}",
    );
    let mut calls = Vec::new();
    for item in &items {
        find_macro_calls(item, &mut calls);
    }
    assert_eq!(calls, vec![("consts", 2), ("twice", 1), ("log", 0), ("y", 2)]);
}

#[test]
fn bang_needs_paren_right_after_it() {
    // `a !(b)` is not a macro call (and not an expression either).
    assert!(Pratt::default().parse("fn f() { let x = a !(b); }").is_err());
    let items = parse_program("fn f() { let x = a!=(b); }");
    let mut calls = Vec::new();
    find_macro_calls(&items[0], &mut calls);
    assert!(calls.is_empty());
}

#[test]
fn format_round_trips_fn_macro_and_call() {
    let src = "macro twice(Expr e) -> Code {\n    return quote expr { ${e} * 2 };\n}\n\nfn main() {\n    let x = twice!(1 + 2, [a, b]);\n    log!();\n}\n";
    let out = crate::format_source(src).expect("format");
    assert_eq!(out, src);
}
