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
