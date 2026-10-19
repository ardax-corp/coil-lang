use super::*;
use crate::typechecking::env::{TyVarCounter, instantiate};
use crate::typechecking::subst::apply_ty_prune;
use crate::typechecking::ty::EnumVariantPayloadTy;
use parser::Pratt;
use parser::SimpleSpan;
use parser::ast::{
    EnumConstructPayload, EnumVariantPayload, LetFieldPattern, LetPattern, MatchArm, PatternField,
    PatternPayload, RecordFieldDecl, RecordFieldValue,
};

#[test]
fn error_with_labels_records_secondary_spans() {
    let mut c = Checker::new();
    let _ = c.error_with_labels(
        ErrorCode::TypeMismatch,
        "mismatch".into(),
        0..1,
        vec![("expected here".into(), 2..4), ("found here".into(), 5..7)],
        Some("see both sites".into()),
    );
    let msgs = c.messages();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].message(), "mismatch");
    assert_eq!(msgs[0].code(), Some(ErrorCode::TypeMismatch));
    assert_eq!(msgs[0].help().as_deref(), Some("see both sites"));
    let labels = msgs[0].labels();
    assert_eq!(labels.len(), 2);
    assert_eq!(labels[0].message(), "expected here");
    assert_eq!(labels[0].range(), &(2..4));
    assert_eq!(labels[1].message(), "found here");
    assert_eq!(labels[1].range(), &(5..7));
}

fn is_bare_expr_source(trimmed: &str) -> bool {
    if trimmed.contains('\n') || trimmed.contains(';') || trimmed.starts_with('{') {
        return false;
    }
    const STMT_PREFIXES: &[&str] = &[
        "let ", "use ", "class ", "enum ", "trait ", "impl ", "test(", "fn ", "if ", "while ",
        "for ", "return ", "async ", "match ", "defer ",
    ];
    !STMT_PREFIXES.iter().any(|p| trimmed.starts_with(p))
}

fn peel_fn_return(ty: Ty) -> Ty {
    match ty {
        Ty::Fun(_, ret) => peel_fn_return(*ret),
        other => other,
    }
}

fn normalize_adjacent_decls(s: &str) -> String {
    let mut out = s.to_string();
    for (from, to) in [
        ("} let ", "}\nlet "),
        ("} fn ", "}\nfn "),
        ("} use ", "}\nuse "),
        ("} class ", "}\nclass "),
        ("} enum ", "}\nenum "),
        ("} impl ", "}\nimpl "),
    ] {
        out = out.replace(from, to);
    }
    out
}

fn block_as_fn_body_return(block: &str) -> Option<String> {
    let inner = block.trim();
    if !inner.starts_with('{') || !inner.ends_with('}') {
        return None;
    }
    let body = inner[1..inner.len() - 1].trim();
    if body.is_empty() {
        return Some(String::new());
    }
    let mut parts = body
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }
    if parts.len() == 1 && parts[0].starts_with('{') {
        return block_as_fn_body_return(parts[0]);
    }
    let last = parts.pop()?;
    let prefix = if parts.is_empty() {
        String::new()
    } else {
        format!("{}; ", parts.join("; "))
    };
    Some(format!("{prefix}return {last};"))
}

fn stmt_tail_probe(src: &str) -> Option<Ty> {
    let trimmed = normalize_adjacent_decls(src.trim());
    let needs_toplevel = trimmed.contains("enum ")
        || trimmed.contains("class ")
        || trimmed.contains("use ")
        || trimmed.contains("impl ");
    let owned = if trimmed.ends_with(';') {
        trimmed
    } else {
        format!("{trimmed};")
    };
    if needs_toplevel {
        let mut c = Checker::new();
        let ast = Pratt::default().parse(owned.as_str()).ok()?;
        let raw = c.check_program(&ast);
        use parser::ast::Expression;
        let Expression::Program(children) = ast.1.as_ref() else {
            return None;
        };
        let last = children.last()?;
        let ty = expr_value_ty(&c, last).unwrap_or_else(|| program_last_expr_ty(&c, &ast, raw));
        return (ty != unit_ty()).then_some(ty);
    }
    let parts = owned
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    if parts.len() < 2 {
        return None;
    }
    let tail = parts.last()?;
    let prefix = parts[..parts.len() - 1].join("; ");
    let wrapped = format!("fn __coil_check_expr__() {{ {prefix}; return {tail}; }}");
    probe_fn_return_type(&wrapped)
}

fn probe_fn_return_type(wrapped: &str) -> Option<Ty> {
    let mut c = Checker::new();
    let ast = Pratt::default().parse(wrapped).ok()?;
    let _ = c.check_program(&ast);
    let scheme = c.env().lookup("__coil_check_expr__")?;
    let mut counter = TyVarCounter::new();
    let fn_ty = apply_ty_prune(c.subst(), &instantiate(scheme, &mut counter));
    Some(peel_fn_return(fn_ty))
}

fn probe_return_type(src: &str) -> Option<Ty> {
    let trimmed = normalize_adjacent_decls(src.trim());
    let expr: String = if is_bare_expr_source(trimmed.as_str()) {
        trimmed
    } else if trimmed == "{}" {
        return None;
    } else if let Some(body) = block_as_fn_body_return(trimmed.as_str()) {
        return probe_fn_return_type(&format!("fn __coil_check_expr__() {{ {body} }}"));
    } else if (trimmed.starts_with("if ") || trimmed.starts_with("if("))
        && trimmed.ends_with('}')
        && !trimmed.contains('\n')
    {
        if trimmed.contains(" else ") {
            return probe_fn_return_type(&format!(
                "fn __coil_check_expr__() {{ return {trimmed}; }}"
            ));
        }
        if let Some(open) = trimmed.find('{') {
            let block = &trimmed[open..];
            if let Some(body) = block_as_fn_body_return(block) {
                let head = &trimmed[..open];
                return probe_fn_return_type(&format!(
                    "fn __coil_check_expr__() {{ {head}{body} }}"
                ));
            }
        }
        return None;
    } else {
        return None;
    };
    probe_fn_return_type(&format!("fn __coil_check_expr__() {{ return {expr}; }}"))
}

fn program_last_expr_ty(c: &Checker, ast: &Output, fallback: Ty) -> Ty {
    use parser::ast::Expression;
    let Expression::Program(children) = ast.1.as_ref() else {
        return fallback;
    };
    let Some(last) = children.last() else {
        return fallback;
    };
    let inner = match last.1.as_ref() {
        Expression::ExprStatement(e) | Expression::Statement(e) => e,
        _ => return fallback,
    };
    c.lookup_for_codegen_span(inner.0.start, inner.0.end)
        .or_else(|| expr_value_ty(c, inner))
        .unwrap_or(fallback)
}

fn expr_value_ty(c: &Checker, expr: &Output) -> Option<Ty> {
    use parser::ast::Expression;
    let span_ty = || c.lookup_for_codegen_span(expr.0.start, expr.0.end);
    match expr.1.as_ref() {
        Expression::Identifier(name) => {
            let scheme = c.env().lookup(name)?;
            let mut counter = TyVarCounter::new();
            Some(apply_ty_prune(
                c.subst(),
                &instantiate(scheme, &mut counter),
            ))
        }
        Expression::ExprStatement(inner) | Expression::Statement(inner) => {
            expr_value_ty(c, inner).or_else(span_ty)
        }
        Expression::Call { .. } | Expression::Access(_, _) => span_ty(),
        _ => span_ty().filter(|ty| *ty != unit_ty()),
    }
}

fn format_check_src(trimmed: &str) -> String {
    if !trimmed.ends_with(';') && !trimmed.ends_with('}') {
        format!("{trimmed};")
    } else {
        trimmed.to_string()
    }
}

/// Parse and infer `src`, returning the checker state and inferred type.
///
/// The top-level parser expects declarations / statements. Bare
/// expressions are wrapped in a probe function so we infer the
/// expression type instead of `unit` from `expr;`.
fn fixture_dload_grants() -> crate::HostGrants {
    let mut g = crate::HostGrants::deny_all();
    for stem in ["sum", "noop", "x.so", "x", "missing", "plugin"] {
        g.grant_dload_allow(stem);
    }
    g
}

fn check(src: &str) -> (Checker, Ty) {
    let mut c = Checker::new();
    c.set_host_grants(fixture_dload_grants(), Vec::new());
    let trimmed = normalize_adjacent_decls(src.trim());
    let owned = format_check_src(trimmed.as_str());
    match Pratt::default().parse(owned.as_str()) {
        Ok(ast) => {
            let ty = c.check_program(&ast);
            (c, ty)
        }
        Err(msg) => panic!("parse failed for `{}`: {:?}", src, msg),
    }
}

fn inferred_expr_ty(src: &str) -> Ty {
    if let Some(ty) = probe_return_type(src) {
        return ty;
    }
    if let Some(ty) = stmt_tail_probe(src) {
        return ty;
    }
    let trimmed = normalize_adjacent_decls(src.trim());
    if trimmed == "{}" {
        return check(src).1;
    }
    let owned = format_check_src(trimmed.as_str());
    let mut c = Checker::new();
    c.set_host_grants(fixture_dload_grants(), Vec::new());
    let ast = Pratt::default()
        .parse(owned.as_str())
        .unwrap_or_else(|msg| panic!("parse failed for `{}`: {:?}", src, msg));
    let raw = c.check_program(&ast);
    program_last_expr_ty(&c, &ast, raw)
}

/// Like `check`, but returns diagnostics instead of asserting none.
fn check_warn(src: &str) -> (Checker, Vec<Message>) {
    let (mut c, _ty) = check(src);
    let msgs = c.take_messages();
    (c, msgs)
}

fn assert_ok(src: &str, expected: Ty) {
    let ty = inferred_expr_ty(src);
    let (mut c, _) = check(src);
    let msgs = c.take_messages();
    assert!(
        msgs.is_empty(),
        "expected no messages for `{}`, got: {:?}",
        src,
        msgs
    );
    assert_eq!(ty, expected, "type mismatch for `{}`", src);
}

fn assert_messages(src: &str) -> Vec<Message> {
    let (mut c, _) = check(src);
    c.take_messages()
}

#[test]
fn let_with_annotation() {
    assert_ok("let x: int = 42;", unit_ty());
}

#[test]
fn let_without_annotation_or_value_uses_fresh_var() {
    // `let x;` ,  x is a fresh type variable (id is not stable across
    // builtin/prelude registration, so only the shape is checked).
    let (mut c, _) = check("let x;");
    assert!(c.take_messages().is_empty());
    let scheme = c.env().lookup("x").unwrap();
    assert!(
        matches!(scheme.ty, Ty::Var(_)),
        "expected a fresh type variable, got {:?}",
        scheme.ty
    );
}

#[test]
fn assignment_updates_existing_var() {
    let (mut c, _) = check("let x; x = 42;");
    assert!(c.take_messages().is_empty());
    let scheme = c.env().lookup("x").unwrap();
    let ty = apply_ty(c.subst(), &scheme.ty);
    assert_eq!(ty, int());
}

#[test]
fn while_loop_returns_unit() {
    assert_ok("while false { 42; }", unit_ty());
}

#[test]
fn empty_block() {
    assert_ok("{}", unit_ty());
}

#[test]
fn defer_returns_unit() {
    assert_ok("defer { 42; }", unit_ty());
}

//      Parser doesn't produce `List` nodes yet, so these are deferred.

#[test]
fn forall_annotation_pretty_or_ty_forall() {
    let (mut c, _) = check("fn app(forall T: Num. T -> T f, int x) -> int { return x; }");
    assert!(c.take_messages().is_empty());
    let scheme = c.env().lookup("app").expect("app should be registered");
    let ty = apply_ty(c.subst(), &scheme.ty);
    let Ty::Fun(param, _) = ty else {
        panic!("expected function type, got {ty}");
    };
    let Ty::Forall {
        bounds,
        constraints,
        body,
    } = param.as_ref()
    else {
        panic!("expected forall parameter, got {param}");
    };
    assert_eq!(bounds.len(), 1);
    assert_eq!(constraints.len(), 1);
    assert_eq!(constraints[0].class, "Num");
    assert!(constraints[0].is_unary_on(bounds[0]));
    assert!(matches!(body.as_ref(), Ty::Fun(_, _)));
    assert!(format!("{}", param).starts_with("forall t"));
}

#[test]
fn class_registers_nominal_constructor() {
    let (mut c, _) = check("class Foo { name: String, }");
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "{:?}", msgs);
    // Foo is a Ty::Con. The fields are stored privately by default.
    let class = c.classes.get("Foo").expect("class not registered");
    assert_eq!(class.len(), 1);
    assert_eq!(class[0].0, Visibility::Private);
    assert_eq!(class[0].1, "name");
    assert_eq!(class[0].2, string());
}

#[test]
fn class_with_pub_field_marks_visibility() {
    let (mut c, _) = check("class Foo { pub age: int, name: String, }");
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "{:?}", msgs);
    let class = c.classes.get("Foo").unwrap();
    // First field is public (pub), second is private.
    assert_eq!(class[0].0, Visibility::Public);
    assert_eq!(class[0].1, "age");
    assert_eq!(class[1].0, Visibility::Private);
    assert_eq!(class[1].1, "name");
}

#[test]
fn class_with_all_private_fields() {
    let (mut c, _) = check("class Foo { x: int, y: int, }");
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "{:?}", msgs);
    let class = c.classes.get("Foo").unwrap();
    assert!(class.iter().all(|(v, _, _)| *v == Visibility::Private));
}

#[test]
fn class_visibility_is_per_field() {
    // First field is public, second is private ,  they're tracked
    // independently even though they live in the same class.
    let (mut c, _) = check("class Foo { pub a: int, b: int, pub c: int, }");
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "{:?}", msgs);
    let fields = &c.classes.get("Foo").unwrap();
    assert_eq!(fields[0].0, Visibility::Public);
    assert_eq!(fields[1].0, Visibility::Private);
    assert_eq!(fields[2].0, Visibility::Public);
}

#[test]
fn class_visibility_recorded_for_future_member_access() {
    let (mut c, _) = check("class Foo { pub age: int, name: String, }");
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "{:?}", msgs);
    let foo = c.classes.get("Foo").unwrap();
    assert_eq!(foo[0].0, Visibility::Public);
    assert_eq!(foo[1].0, Visibility::Private);
    assert!(Checker::can_access_member(Visibility::Public, "Foo", None));
    assert!(!Checker::can_access_member(
        Visibility::Private,
        "Foo",
        None
    ));
    assert!(Checker::can_access_member(
        Visibility::Private,
        "Foo",
        Some("Foo")
    ));
}

#[test]
fn generic_class_fields_stored_as_param_placeholders() {
    let (mut c, _) = check("class Cell<T> { pub value: T }");
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "{:?}", msgs);
    let fields = c.classes.get("Cell").expect("Cell registered");
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].1, "value");
    assert_eq!(fields[0].2, Ty::Con("T".into()));
}

#[test]
fn recursive_method_via_self_binding() {
    // fib uses no self, but `==` is the only comparison the
    // parser currently supports; use that for the branch.
    let src = "impl Counter { pub fn tick(int n) -> int { if n == 0 { return 0; } return tick(n - 1) + 1; } }";
    let (mut c, _) = check(src);
    // We don't require no messages ,  the outer call site `tick(...)`
    // may have residual issues ,  but the method should be registered.
    let _ = c.take_messages();
    assert!(c.methods.get("Counter").unwrap().contains_key("tick"));
}

#[test]
fn register_native_adds_function_to_env() {
    let mut c = Checker::new();
    c.register_native("add", &[int(), int()], &int());
    let scheme = c.env().lookup("add").expect("add not registered");
    let ty = apply_ty(c.subst(), &scheme.ty);
    // Curried: int -> int -> int
    assert_eq!(
        ty,
        Ty::Fun(
            Box::new(int()),
            Box::new(Ty::Fun(Box::new(int()), Box::new(int())))
        )
    );
}

#[test]
fn register_native_no_args() {
    let mut c = Checker::new();
    c.register_native("now", &[], &int());
    let scheme = c.env().lookup("now").expect("now not registered");
    let ty = apply_ty(c.subst(), &scheme.ty);
    assert_eq!(ty, int());
}

#[test]
fn register_native_void_return() {
    let mut c = Checker::new();
    c.register_native("print", &[string()], &unit_ty());
    let scheme = c.env().lookup("print").expect("print not registered");
    let ty = apply_ty(c.subst(), &scheme.ty);
    // string -> unit
    assert_eq!(ty, Ty::Fun(Box::new(string()), Box::new(unit_ty())));
}

#[test]
fn register_native_object_type() {
    let mut c = Checker::new();
    c.register_native("make_foo", &[], &Ty::Con("Foo".into()));
    let scheme = c.env().lookup("make_foo").expect("make_foo not registered");
    let ty = apply_ty(c.subst(), &scheme.ty);
    assert_eq!(ty, Ty::Con("Foo".into()));
}

#[test]
fn infer_depth_guard_panics_with_expected_diagnostic_past_limit() {
    // Exercise the guard directly (not via a literal deeply-nested AST):
    // recursing 2000+ levels via `+` also overflows the stack while
    // dropping the parsed AST itself (a well-known Box<T> recursive-Drop
    // pitfall unrelated to infer_inner's own frame size), so a real
    // pathologically-deep program isn't a safe way to test this in
    // isolation. Seed `infer_depth` to the limit and confirm the very
    // next `infer` call panics with a clean diagnostic instead of
    // recursing further ,  this is the same code path a real deeply
    // nested expression would hit.
    let mut c = Checker::new();
    let ast = Pratt::default()
        .parse("1;")
        .expect("trivial literal parses");
    c.infer_depth = INFER_RECURSION_LIMIT;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.infer(&ast)));
    assert!(result.is_err(), "expected the recursion-limit panic");
    assert!(
        c.messages()
            .iter()
            .any(|m| m.code() == Some(ErrorCode::ExpressionNestingTooDeep)),
        "expected an ExpressionNestingTooDeep diagnostic to be recorded before panicking"
    );
}

#[test]
fn native_function_call_infers_correctly() {
    // After register_native, a call to the native should type-check
    // against the registered signature and produce the right type.
    let mut c = Checker::new();
    c.register_native("print", &[string()], &unit_ty());
    let ast = Pratt::default().parse("print(\"hi\");").expect("parse");
    let ty = c.check_program(&ast);
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "{:?}", msgs);
    assert_eq!(ty, unit_ty());
}

#[test]
fn native_function_arity_mismatch_errors() {
    // print takes 1 arg; call with 2.
    let mut c = Checker::new();
    c.register_native("print", &[string()], &unit_ty());
    let ast = Pratt::default()
        .parse("print(\"a\", \"b\");")
        .expect("parse");
    let _ = c.check_program(&ast);
    let msgs = c.take_messages();
    assert!(!msgs.is_empty(), "expected arity-mismatch error");
}

#[test]
fn native_function_call_with_correct_arity_succeeds() {
    // Once registered, a function call with matching arity and types
    // type-checks cleanly.
    let mut c = Checker::new();
    c.register_native("add", &[int(), int()], &int());
    let ast = Pratt::default().parse("add(1, 2);").expect("parse");
    let _ = c.check_program(&ast);
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "{:?}", msgs);
}

#[test]
fn native_visible_inside_nested_block() {
    // Natives registered on the checker are visible from any
    // nested scope.
    let mut c = Checker::new();
    c.register_native("print", &[string()], &unit_ty());
    let ast = Pratt::default().parse("{ print(\"a\"); }").expect("parse");
    let _ = c.check_program(&ast);
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "{:?}", msgs);
}

//
// The following tests verify that emitted `Message`s are well-formed
// for ariadne: each carries a clear headline, a primary label at
// the error range, and (where helpful) a `help` hint with extra
// context. ariadne's renderer consumes exactly this shape, so as
// long as the `Message` fields are populated, the diagnostic will
// display cleanly.

#[test]
fn unknown_identifier_message_uses_can_not_find_format() {
    let (mut c, _) = check("unknown_var;");
    let msgs = c.take_messages();
    assert_eq!(msgs.len(), 1);
    let msg = &msgs[0];
    assert!(
        msg.message().contains("Cannot find value"),
        "got: {:?}",
        msg.message()
    );
    assert!(
        msg.message().contains("unknown_var"),
        "got: {:?}",
        msg.message()
    );
    // Primary range is set (ariadne uses this for the underline).
    let r = msg.range();
    assert!(r.start <= r.end, "bad range {:?}", r);
    assert!(r.end > 0, "expected non-empty range");
}

#[test]
fn type_mismatch_message_uses_expected_actual_format() {
    let (mut c, _) = check("let x: int = \"hello\";");
    let msgs = c.take_messages();
    // The Fragment return is unit, so the assignment's type
    // mismatch is the only diagnostic.
    assert!(!msgs.is_empty(), "expected at least one message");
    let msg = msgs.iter().find(|m| m.message().contains("Type mismatch"));
    assert!(
        msg.is_some(),
        "no type-mismatch message found in {:?}",
        msgs
    );
    let msg = msg.unwrap();
    assert!(
        msg.message().contains("expected"),
        "got: {:?}",
        msg.message()
    );
    assert!(msg.message().contains("found"), "got: {:?}", msg.message());
    assert!(msg.message().contains("int"), "got: {:?}", msg.message());
    assert!(msg.message().contains("string"), "got: {:?}", msg.message());
    // Help is present (the context).
    assert!(msg.help().is_some(), "missing help");
    let help = msg.help().as_ref().unwrap();
    assert!(help.contains("let binding"), "got help: {:?}", help);
}

#[test]
fn not_a_function_message_uses_cannot_call_format() {
    // `let x = 5; x(2);` ,  `x` is an int, calling it is an error.
    let (mut c, _) = check("let x = 5; x(2);");
    let msgs = c.take_messages();
    assert!(!msgs.is_empty(), "expected a message");
    let msg = msgs
        .iter()
        .find(|m| m.message().contains("`x` is not a function: it has type `int`"));
    assert!(msg.is_some(), "got: {:?}", msgs);
}

#[test]
fn logical_operand_mismatch_expects_bool() {
    // #618: the operand is the `int`; `&&` expects `bool`.
    let (mut c, _) = check("let a = 1 && 2;");
    let msgs = c.take_messages();
    assert!(
        msgs.iter()
            .any(|m| m.message().contains("expected `bool`, found `int`")),
        "got: {:?}",
        msgs
    );
    assert!(
        !msgs
            .iter()
            .any(|m| m.message().contains("expected `int`, found `bool`")),
        "reversed mismatch: {:?}",
        msgs
    );
}

#[test]
fn optional_access_on_result_prints_result_type() {
    // #618: the structural sum of a `Result::Ok` construct prints as written.
    let (mut c, _) = check("let r = Result::Ok({ v: 1 }); let x = r?.v;");
    let msgs = c.take_messages();
    let msg = msgs
        .iter()
        .find(|m| m.message().contains("`?.` requires Option"))
        .unwrap_or_else(|| panic!("no `?.` diagnostic: {msgs:?}"));
    assert!(
        msg.message().contains("found `Result<{ v: int }, a>`"),
        "got: {}",
        msg.message()
    );
}

#[test]
fn unknown_function_message_uses_can_not_find_format() {
    let (mut c, _) = check("missing_fn();");
    let msgs = c.take_messages();
    assert!(!msgs.is_empty(), "expected a message");
    let msg = msgs
        .iter()
        .find(|m| m.message().contains("Cannot find function"));
    assert!(msg.is_some(), "got: {:?}", msgs);
    let msg = msg.unwrap();
    assert!(
        msg.message().contains("`missing_fn`"),
        "got: {:?}",
        msg.message()
    );
}

#[test]
fn assignment_to_undeclared_message_includes_help_hint() {
    let (mut c, _) = check("undeclared = 1;");
    let msgs = c.take_messages();
    assert!(!msgs.is_empty(), "expected a message");
    let msg = msgs
        .iter()
        .find(|m| m.message().contains("Cannot assign to undeclared"));
    assert!(msg.is_some(), "got: {:?}", msgs);
    let msg = msg.unwrap();
    let help = msg.help().as_ref().expect("missing help");
    assert!(
        help.contains("let undeclared"),
        "help should suggest `let undeclared;`, got: {:?}",
        help
    );
}

#[test]
fn arity_mismatch_message_mentions_function_name() {
    // foo takes 1 arg; call with 2.
    let mut c = Checker::new();
    c.register_native("foo", &[int()], &int());
    let ast = Pratt::default().parse("foo(1, 2);").expect("parse");
    let _ = c.check_program(&ast);
    let msgs = c.take_messages();
    assert!(!msgs.is_empty(), "expected a message");
    let msg = msgs
        .iter()
        .find(|m| m.message().contains("too many arguments"));
    assert!(msg.is_some(), "got: {:?}", msgs);
    let msg = msg.unwrap();
    assert!(msg.message().contains("`foo`"), "got: {:?}", msg.message());
}

#[test]
fn diagnostic_messages_have_valid_ranges() {
    // Every diagnostic should have a non-empty range that lies
    // within the source bounds (0..src.len()). ariadne's renderer
    // requires this.
    for src in &["x;", "1 + true", "let y: int = 1; y = \"z\";"] {
        let (mut c, _) = check(src);
        let src_len = src.len();
        for msg in c.take_messages() {
            let r = msg.range();
            assert!(
                r.start <= r.end && r.end <= src_len,
                "bad range {:?} for source len {} (msg: {})",
                r,
                src_len,
                msg.message()
            );
        }
    }
}

#[test]
fn pattern_error_span_points_at_arm_body() {
    // Regression test for the `0..0` pattern-error span bug.
    // Pattern errors used to land at byte 0 of the source because
    // `expected_ty_span_range` always returned `0..0`. After
    // threading `arm.body.0.into_range()` through `infer_pattern`,
    // the diagnostic for a wrong-arity pattern should anchor
    // somewhere inside the source ,  NOT at byte 0.
    let src = "let x = Option::Some(1); match x { Option::Some(a, b) => 0 };";
    let (mut c, _) = check(src);
    let src_len = src.len();
    let msgs = c.take_messages();
    assert!(
        !msgs.is_empty(),
        "expected at least one diagnostic for `{}`",
        src
    );
    // The wrong-arity error from `infer_pattern` must NOT be
    // at byte 0.
    let arity_msg = msgs
        .iter()
        .find(|m| m.message().contains("expects"))
        .expect("expected a wrong-arity pattern diagnostic");
    let r = arity_msg.range();
    assert!(
        r.start > 0,
        "pattern diagnostic anchored at byte 0 — `0..0` regression: \
             range={:?} msg={:?} src={:?}",
        r,
        arity_msg.message(),
        src
    );
    assert!(
        r.end <= src_len,
        "pattern diagnostic range {:?} exceeds source length {}",
        r,
        src_len
    );
}

#[test]
fn multiple_natives_coexist() {
    let mut c = Checker::new();
    c.register_native("print", &[string()], &unit_ty());
    c.register_native("add", &[int(), int()], &int());
    c.register_native("now", &[], &string());

    let print_ty = apply_ty(c.subst(), &c.env().lookup("print").unwrap().ty);
    let add_ty = apply_ty(c.subst(), &c.env().lookup("add").unwrap().ty);
    let now_ty = apply_ty(c.subst(), &c.env().lookup("now").unwrap().ty);

    assert_eq!(print_ty, Ty::Fun(Box::new(string()), Box::new(unit_ty())));
    assert_eq!(
        add_ty,
        Ty::Fun(
            Box::new(int()),
            Box::new(Ty::Fun(Box::new(int()), Box::new(int())))
        )
    );
    assert_eq!(now_ty, string());
}

#[test]
fn free_fn_arg_node_ids_cached() {
    let src = "fn f(int x) -> int { return x; }";
    let (c, _) = check(src);
    assert!(c.messages().is_empty(), "{:?}", c.messages());
    assert_eq!(
        c.codegen_var_type("x")
            .map(|t| apply_ty_prune(c.subst(), t)),
        Some(int())
    );
    let ast = Pratt::default().parse(src).expect("parse");
    fn find_fn_arg_span(node: &parser::ast::Output<'_>) -> Option<(usize, usize)> {
        use parser::ast::Expression;
        match node.1.as_ref() {
            Expression::Program(cs) | Expression::Block(cs) | Expression::Fragment(cs) => {
                cs.iter().find_map(find_fn_arg_span)
            }
            Expression::Function { args, body, .. } => {
                if let Expression::Fragment(children) = args.1.as_ref() {
                    let found = children.iter().find_map(|child| {
                        if matches!(child.1.as_ref(), Expression::Argument { .. }) {
                            Some((child.0.start, child.0.end))
                        } else {
                            None
                        }
                    });
                    if found.is_some() {
                        return found;
                    }
                }
                body.as_ref().and_then(find_fn_arg_span)
            }
            Expression::Expr(e)
            | Expression::Statement(e)
            | Expression::ExprStatement(e)
            | Expression::Group(e) => find_fn_arg_span(e),
            _ => None,
        }
    }
    let (start, end) = find_fn_arg_span(&ast).expect("free-fn Argument span");
    assert_eq!(
        c.lookup_for_codegen_span(start, end),
        Some(int()),
        "free-fn Argument must be in the NodeId cache / span table"
    );
    let side = c.typed_sidecar();
    assert!(
        side.tys().values().any(|t| t == &int()),
        "sidecar must record the parameter type by NodeId"
    );
}

#[test]
fn typed_sidecar_is_nodeid_keyed() {
    let src = "fn f(int x) -> int { return x; }";
    let (c, _) = check(src);
    assert!(c.messages().is_empty(), "{:?}", c.messages());
    let side = c.typed_sidecar();
    assert!(!side.tys().is_empty(), "sidecar tys after check_program");
    let f_id = c.def_id_of("f").expect("f DefId");
    assert!(
        c.id_table()
            .ids()
            .iter()
            .any(|id| c.def_id_at(*id) == Some(f_id)),
        "Function node must carry DefId"
    );
    assert_eq!(
        side.def_id(
            *c.id_table()
                .ids()
                .iter()
                .find(|id| c.def_id_at(**id) == Some(f_id))
                .unwrap()
        ),
        Some(f_id)
    );
}

#[test]
fn lambda_arg_node_ids_cached_before_body() {
    let src = "let f = fn (int x) => x; let _ = f(1);";
    let (c, _) = check(src);
    assert!(c.messages().is_empty(), "{:?}", c.messages());
    let ast = Pratt::default().parse(src).expect("parse");

    fn find_lambda_arg_span(node: &parser::ast::Output<'_>) -> Option<(usize, usize)> {
        use parser::ast::Expression;
        match node.1.as_ref() {
            Expression::Program(cs) | Expression::Block(cs) | Expression::Fragment(cs) => {
                cs.iter().find_map(find_lambda_arg_span)
            }
            Expression::Variable(_, Some(value)) | Expression::Constant(_, Some(value)) => {
                find_lambda_arg_span(value)
            }
            Expression::Lambda { args, .. } => {
                if let Expression::Fragment(children) = args.1.as_ref() {
                    children.iter().find_map(|child| {
                        if matches!(child.1.as_ref(), Expression::Argument { .. }) {
                            Some((child.0.start, child.0.end))
                        } else {
                            None
                        }
                    })
                } else {
                    None
                }
            }
            Expression::Expr(e)
            | Expression::Statement(e)
            | Expression::ExprStatement(e)
            | Expression::Group(e) => find_lambda_arg_span(e),
            _ => None,
        }
    }

    let (start, end) = find_lambda_arg_span(&ast).expect("lambda Argument span");
    assert_eq!(
        c.lookup_for_codegen_span(start, end),
        Some(int()),
        "lambda Argument span must cache the parameter type"
    );
}

#[test]
fn lambda_multi_arg_node_ids_cached() {
    let src = "let f = fn (int x, int y) => x + y; let _ = f(1, 2);";
    let (c, _) = check(src);
    assert!(c.messages().is_empty(), "{:?}", c.messages());
    let ast = Pratt::default().parse(src).expect("parse");

    fn collect_lambda_arg_spans(node: &parser::ast::Output<'_>) -> Vec<(usize, usize)> {
        use parser::ast::Expression;
        let mut out = Vec::new();
        match node.1.as_ref() {
            Expression::Program(cs) | Expression::Block(cs) | Expression::Fragment(cs) => {
                for c in cs {
                    out.extend(collect_lambda_arg_spans(c));
                }
            }
            Expression::Variable(_, Some(value)) | Expression::Constant(_, Some(value)) => {
                out.extend(collect_lambda_arg_spans(value));
            }
            Expression::Lambda { args, .. } => {
                if let Expression::Fragment(children) = args.1.as_ref() {
                    for child in children {
                        if matches!(child.1.as_ref(), Expression::Argument { .. }) {
                            out.push((child.0.start, child.0.end));
                        }
                    }
                }
            }
            Expression::Expr(e)
            | Expression::Statement(e)
            | Expression::ExprStatement(e)
            | Expression::Group(e) => out.extend(collect_lambda_arg_spans(e)),
            _ => {}
        }
        out
    }

    let spans = collect_lambda_arg_spans(&ast);
    assert_eq!(spans.len(), 2, "expected two Argument nodes");
    for (start, end) in spans {
        assert_eq!(
            c.lookup_for_codegen_span(start, end),
            Some(int()),
            "each lambda Argument span must cache int"
        );
    }
}

#[test]
fn nested_lambda_arg_node_ids_cached() {
    // Nested lambdas both consume Fragment+Argument NodeIds; body cache
    // must stay lockstep (no Identifier span prefer for args).
    let src = "let f = fn (int x) => fn (int y) use (x) => x + y; let g = f(1); let _ = g(2);";
    let (c, _) = check(src);
    assert!(c.messages().is_empty(), "{:?}", c.messages());
    let ast = Pratt::default().parse(src).expect("parse");

    fn collect_lambda_arg_spans(node: &parser::ast::Output<'_>) -> Vec<(usize, usize)> {
        use parser::ast::Expression;
        let mut out = Vec::new();
        match node.1.as_ref() {
            Expression::Program(cs) | Expression::Block(cs) | Expression::Fragment(cs) => {
                for c in cs {
                    out.extend(collect_lambda_arg_spans(c));
                }
            }
            Expression::Variable(_, Some(value)) | Expression::Constant(_, Some(value)) => {
                out.extend(collect_lambda_arg_spans(value));
            }
            Expression::Lambda { args, body, .. } => {
                if let Expression::Fragment(children) = args.1.as_ref() {
                    for child in children {
                        if matches!(child.1.as_ref(), Expression::Argument { .. }) {
                            out.push((child.0.start, child.0.end));
                        }
                    }
                }
                out.extend(collect_lambda_arg_spans(body));
            }
            Expression::Expr(e)
            | Expression::Statement(e)
            | Expression::ExprStatement(e)
            | Expression::Group(e)
            | Expression::ImplicitReturn(e)
            | Expression::Return(e) => out.extend(collect_lambda_arg_spans(e)),
            Expression::Call { name, args } => {
                out.extend(collect_lambda_arg_spans(name));
                if let Some(args) = args {
                    for a in args {
                        out.extend(collect_lambda_arg_spans(a));
                    }
                }
            }
            Expression::Add(a, b) | Expression::Sub(a, b) | Expression::Mul(a, b) => {
                out.extend(collect_lambda_arg_spans(a));
                out.extend(collect_lambda_arg_spans(b));
            }
            _ => {}
        }
        out
    }

    let spans = collect_lambda_arg_spans(&ast);
    assert_eq!(spans.len(), 2, "outer and inner Argument spans");
    for (start, end) in spans {
        assert_eq!(
            c.lookup_for_codegen_span(start, end),
            Some(int()),
            "nested lambda Argument spans must cache int"
        );
    }
}

#[test]
fn cache_is_populated_after_check_program() {
    // After infer, every pre-walked node should have a cached type.
    let (mut c, _) = check("1 + 2;");
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "{:?}", msgs);
    let total = c.id_table().len();
    assert!(total > 0);
    assert_eq!(c.cache_len(), total);
}

#[test]
fn cache_lookup_returns_inferred_type() {
    // `1 + 2` parses to Expr(Add(Integer, Integer)); we expect the
    // cache to hold int() for each of those nodes.
    let (c, _) = check("1 + 2");
    let ids = c.id_table().ids();
    for id in ids {
        let ty = c
            .lookup_at(*id)
            .unwrap_or_else(|| panic!("no cache entry for {:?}", id));
        if ty == unit_ty() || ty == never() {
            continue;
        }
        assert_eq!(ty, int(), "node {:?} had type {}", id, ty);
    }
}

#[test]
fn cache_lookup_applies_substitution() {
    // `let x = 42; x` unifies x's fresh var with int. After unify,
    // the cached type for `x` should resolve to int, not the
    // original Ty::Var.
    let (mut c, _) = check("let x = 42; x");
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "{:?}", msgs);
    let scheme = c.env().lookup("x").expect("x not bound").clone();
    let resolved = apply_ty_prune(c.subst(), &scheme.ty);
    assert_eq!(resolved, int());
}

#[test]
fn fn_param_ids_do_not_overwrite_other_nodes() {
    // Parameter types used to be cached under the raw pre-order counter id.
    // Once the counter drifted, a later `float x` param landed on `lcm`'s
    // int `a`, and codegen compiled `a * b` to MULF.
    // Windows checkouts may carry CRLF; spans below assume `\n`.
    let src = include_str!("../../../../tests/functions/overload_int_mul_after_float.hy")
        .replace("\r\n", "\n");
    let src = src.as_str();
    let mut c = Checker::new();
    let ast = Pratt::default().parse(src).expect("parse");
    c.check_program(&ast);
    let start = src.find("fn lcm").expect("lcm");
    let end = start + src[start..].find("\n}\n").expect("lcm end");
    let mut seen = 0;
    for id in c.id_table().ids() {
        let Some((s, e)) = c.id_table().span_of(*id) else {
            continue;
        };
        if s >= start && e <= end {
            seen += 1;
            assert_ne!(
                c.lookup_at(*id),
                Some(float()),
                "node {id:?} at {s}..{e} (`{}`) in int-only lcm typed float",
                &src[s..e]
            );
        }
    }
    assert!(seen > 10, "expected lcm's nodes to be visited, saw {seen}");
}

#[test]
fn cache_lookup_returns_none_for_unknown_id() {
    let (c, _) = check("42;");
    assert!(c.lookup_at(NodeId(9999)).is_none());
}

#[test]
fn pre_walk_mints_distinct_ids_for_nodes_sharing_a_span() {
    // `42;` produces a Program and a Statement that both span the
    // entire source. The pre-walk must give them distinct IDs.
    let (mut c, _) = check("42;");
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "{:?}", msgs);
    let ids = c.id_table().ids();
    let unique: std::collections::HashSet<_> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len(), "IDs must be unique per AST node");
    assert!(ids.len() >= 3);
}

#[test]
fn pre_walk_is_deterministic() {
    let (mut c1, _) = check("let a = 1; let b = 2; a + b");
    let (mut c2, _) = check("let a = 1; let b = 2; a + b");
    let msgs1 = c1.take_messages();
    let msgs2 = c2.take_messages();
    assert!(msgs1.is_empty(), "{:?}", msgs1);
    assert!(msgs2.is_empty(), "{:?}", msgs2);
    assert_eq!(c1.id_table().len(), c2.id_table().len());
    assert_eq!(c1.id_table().ids(), c2.id_table().ids());
}

#[test]
fn cache_has_entries_for_value_producing_nodes() {
    // The cache holds a type per node that produces a value.
    // Declarations like `Variable` and `Comment` are side effects on
    // the env and don't produce a typed value, so they don't get a
    // cache entry ,  but they're still visited by the pre-walk.
    // The cache size should therefore be `<=` the pre-walk size.
    for src in &["42;", "1 + 2;", "let x = 1; x", "if true { 42; }"] {
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?} for `{}`", msgs, src);
        assert!(
            c.cache_len() <= c.id_table().len(),
            "cache ({}) larger than pre-walk ({}) for `{}`",
            c.cache_len(),
            c.id_table().len(),
            src
        );
        // And at least one entry per source.
        assert!(c.cache_len() > 0, "empty cache for `{}`", src);
    }
}

#[test]
fn enum_decl_registers_sum_type() {
    let (mut c, _) = check("enum E { A, B }");
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "{:?}", msgs);
    assert!(c.enums.contains_key("E"));
    let variants = c.enums.get("E").unwrap();
    assert_eq!(variants, &vec!["A".to_string(), "B".to_string()]);
}

#[test]
fn enum_with_payload_registers_constructor() {
    // After registration, `Box::Full` is bound as a curried
    // function in the env: `int -> Constructor`.
    let (mut c, _) = check("enum Box { Empty, Full(int) }");
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "{:?}", msgs);
    let scheme = c.env().lookup("Box::Full").expect("not bound");
    let ty = apply_ty_prune(c.subst(), &scheme.ty);
    assert_eq!(
        ty,
        Ty::Fun(
            Box::new(int()),
            Box::new(Ty::Constructor {
                owner: Box::new(Ty::Sum {
                    name: "Box".into(),
                    variants: vec![
                        ("Empty".into(), EnumVariantPayloadTy::Unit),
                        ("Full".into(), EnumVariantPayloadTy::Tuple(vec![int()])),
                    ],
                }),
                tag: 1,
                arity: 1,
            }),
        )
    );
}

#[test]
fn enum_tags_assigned_in_declaration_order() {
    // Tags follow source-declaration order, not alphabetical.
    // `enum E { Z, A, M, B }` → Z=0, A=1, M=2, B=3.
    let (mut c, _) = check("enum E { Z, A, M, B }");
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "{:?}", msgs);
    assert_eq!(c.tag_for("E", "Z"), Some(0));
    assert_eq!(c.tag_for("E", "A"), Some(1));
    assert_eq!(c.tag_for("E", "M"), Some(2));
    assert_eq!(c.tag_for("E", "B"), Some(3));
}

#[test]
fn enum_decl_cache_aligned_with_id_table() {
    // `infer_enum_decl` must consume the same NodeIds as the pre-walk
    // (EnumDecl + variants + payload Type nodes) so cache and id_table match.
    for src in &[
        "enum Color { Red, Green(int) }",
        "enum E { A, B, C }",
        "enum Tree { Leaf, Node(int, Tree, Tree) }",
    ] {
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?} for `{}`", msgs, src);
        assert_eq!(
            c.cache_len(),
            c.id_table().len(),
            "cache ({}) and id_table ({}) out of sync for `{}` \
                 — `infer_enum_decl` is not consuming every pre-walked ID",
            c.cache_len(),
            c.id_table().len(),
            src,
        );
        // Sanity: every pre-walked ID has a cached entry
        // (cache_len == id_table.len() already implies this,
        // but make the intent explicit).
        for id in c.id_table().ids() {
            assert!(
                c.lookup_at(*id).is_some(),
                "pre-walked ID {:?} has no cache entry for `{}`",
                id,
                src,
            );
        }
    }
}

#[test]
fn constructor_call_with_correct_arity_typechecks() {
    let src = "Option::Some(42)";
    let (mut c, _) = check(src);
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "{:?}", msgs);
    // Check via the cache that the call produced a Constructor type.
    let ids = c.id_table().ids();
    let found = ids.iter().find_map(|id| match c.lookup_at(*id) {
        Some(Ty::Constructor { tag, arity, .. }) => Some((tag, arity)),
        _ => None,
    });
    assert_eq!(found, Some((1, 1)));
}

#[test]
fn dict_duplicate_field_emits_diagnostic() {
    // Parser rejects `{ foo: 1, foo: 2 }`; typecheck still reports if
    // parse is bypassed.
    let span = SimpleSpan::from(0..1);
    let node = |e: Expression<'static>| (span, Box::new(e));
    let int = |n: i64| node(Expression::Integer(n));
    let ast = node(Expression::Dict(vec![
        RecordFieldValue {
            name: "foo",
            value: int(1),
        },
        RecordFieldValue {
            name: "foo",
            value: int(2),
        },
    ]));
    let mut c = Checker::new();
    let _ = c.check_program(&ast);
    let msgs = c.take_messages();
    let found = msgs.iter().any(|m| {
        m.code() == Some(ErrorCode::DuplicateField) && m.message().contains("Duplicate field")
    });
    assert!(
        found,
        "expected duplicate-field diagnostic, got: {:?}",
        msgs
    );
}

#[test]
fn let_record_duplicate_field_emits_diagnostic_if_parse_bypassed() {
    let span = SimpleSpan::from(0..1);
    let node = |e: Expression<'static>| (span, Box::new(e));
    let int = |n: i64| node(Expression::Integer(n));
    let ast = node(Expression::LetDestructure {
        pattern: LetPattern::Record(vec![
            LetFieldPattern {
                name: "x",
                pattern: LetPattern::Binding { name: "a" },
            },
            LetFieldPattern {
                name: "x",
                pattern: LetPattern::Binding { name: "b" },
            },
        ]),
        rhs: node(Expression::Dict(vec![
            RecordFieldValue {
                name: "x",
                value: int(1),
            },
            RecordFieldValue {
                name: "y",
                value: int(2),
            },
        ])),
    });
    let mut c = Checker::new();
    let _ = c.check_program(&ast);
    let msgs = c.take_messages();
    let found = msgs.iter().any(|m| {
        m.code() == Some(ErrorCode::DuplicateField)
            && m.message()
                .contains("Duplicate field `x` in record pattern")
    });
    assert!(
        found,
        "expected duplicate-field diagnostic, got: {:?}",
        msgs
    );
}

#[test]
fn record_construct_duplicate_field_emits_diagnostic_if_parse_bypassed() {
    let span = SimpleSpan::from(0..1);
    let node = |e: Expression<'static>| (span, Box::new(e));
    let int = |n: i64| node(Expression::Integer(n));
    let ty_int = node(Expression::Type("int"));
    let enum_decl = node(Expression::EnumDecl {
        docs: vec![],
        attrs: vec![],
        name: "E",
        type_params: vec![],
        variants: vec![node(Expression::EnumVariant {
            docs: vec![],
            attrs: vec![],
            name: "Foo",
            payload: EnumVariantPayload::Record(vec![
                RecordFieldDecl {
                    name: "x",
                    value: ty_int.clone(),
                },
                RecordFieldDecl {
                    name: "y",
                    value: ty_int,
                },
            ]),
            discriminant: None,
        })],
    });
    let construct = node(Expression::Construct {
        enum_name: "E",
        variant_name: "Foo",
        fields: EnumConstructPayload::Record(vec![
            RecordFieldValue {
                name: "x",
                value: int(1),
            },
            RecordFieldValue {
                name: "x",
                value: int(2),
            },
        ]),
    });
    let ast = node(Expression::Program(vec![enum_decl, construct]));
    let mut c = Checker::new();
    let _ = c.check_program(&ast);
    let msgs = c.take_messages();
    let found = msgs.iter().any(|m| {
        m.code() == Some(ErrorCode::DuplicateField)
            && m.message()
                .contains("Duplicate field `x` in record constructor")
    });
    assert!(
        found,
        "expected duplicate-field diagnostic, got: {:?}",
        msgs
    );
}

#[test]
fn record_pattern_duplicate_field_emits_diagnostic_if_parse_bypassed() {
    let span = SimpleSpan::from(0..1);
    let node = |e: Expression<'static>| (span, Box::new(e));
    let ty_int = node(Expression::Type("int"));
    let enum_decl = node(Expression::EnumDecl {
        docs: vec![],
        attrs: vec![],
        name: "P",
        type_params: vec![],
        variants: vec![node(Expression::EnumVariant {
            docs: vec![],
            attrs: vec![],
            name: "P",
            payload: EnumVariantPayload::Record(vec![
                RecordFieldDecl {
                    name: "x",
                    value: ty_int.clone(),
                },
                RecordFieldDecl {
                    name: "y",
                    value: ty_int,
                },
            ]),
            discriminant: None,
        })],
    });
    let binding = |name| (span, Pattern::Binding { name });
    let match_expr = node(Expression::Match {
        scrutinee: node(Expression::Identifier("p")),
        arms: vec![MatchArm {
            pattern: (
                span,
                Pattern::Constructor {
                    enum_name: "P",
                    variant_name: "P",
                    payload: PatternPayload::Record(vec![
                        PatternField {
                            name: "x",
                            pattern: binding("x"),
                        },
                        PatternField {
                            name: "x",
                            pattern: binding("x"),
                        },
                    ]),
                },
            ),
            body: node(Expression::Identifier("x")),
        }],
    });
    let ast = node(Expression::Program(vec![enum_decl, match_expr]));
    let mut c = Checker::new();
    let _ = c.check_program(&ast);
    let msgs = c.take_messages();
    let found = msgs.iter().any(|m| {
        m.code() == Some(ErrorCode::DuplicateField) && m.message().contains("Duplicate field")
    });
    assert!(
        found,
        "expected duplicate-field diagnostic, got: {:?}",
        msgs
    );
}

#[test]
fn builtin_option_type_app_builds_ty_app() {
    let src = "fn f(Option<int> x) -> int { return 0; }";
    let (mut c, _) = check(src);
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    assert_eq!(
        c.generics
            .generic_type_ctors
            .get(common::BUILTIN_OPTION_ENUM),
        Some(&vec!["T".to_string()])
    );
    assert_eq!(
        c.generics
            .generic_type_ctors
            .get(common::BUILTIN_RESULT_ENUM),
        Some(&vec!["T".to_string(), "E".to_string()])
    );

    let scheme = c.env().lookup("f").unwrap();
    let ty = apply_ty_prune(c.subst(), &scheme.ty);
    let Ty::Fun(param, ret) = ty else {
        panic!("expected function type");
    };
    assert_eq!(
        *param,
        Ty::App(
            Box::new(Ty::Con(common::BUILTIN_OPTION_ENUM.into())),
            vec![int()]
        )
    );
    assert!(is_option_ty(&param));
    assert_eq!(option_inner(&param), Some(int()));
    assert_eq!(*ret, int());
}

#[test]
fn ambiguous_instance_discharge_reports_error() {
    let mut c = Checker::new();
    c.generics.instances.clear();
    c.generics.instances.push(InstanceDef {
        class: "Choice".to_string(),
        defined_module: "a".to_string(),
        range: 0..1,
        args: vec![Ty::Var(TyVarId(999))],
        method_fqns: HashMap::new(),
        assoc_tys: HashMap::new(),
        context: Vec::new(),
    });
    c.generics.instances.push(InstanceDef {
        class: "Choice".to_string(),
        defined_module: "b".to_string(),
        range: 1..2,
        args: vec![int()],
        method_fqns: HashMap::new(),
        assoc_tys: HashMap::new(),
        context: Vec::new(),
    });

    c.discharge_constraints(
        None,
        &[Constraint {
            class: "Choice".to_string(),
            args: vec![int()],
        }],
        &(0..2),
    );

    assert!(
        c.messages()
            .iter()
            .any(|m| m.message().contains("Ambiguous instance for `Choice<int>`")),
        "expected ambiguous-instance diagnostic, got: {:?}",
        c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
    );
}

#[test]
fn typeclass_impl_default_method_omission_registers_default_fqn() {
    let src = r#"
            trait Tiny<T> {
                fn zero() -> int { return 0; }
            }
            impl Tiny for int {
            }
        "#;
    let (c, msgs) = check_warn(src);
    assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    assert_eq!(
        c.instance_method_fqn("Tiny", &[int()], "zero"),
        Some("Tiny__default__zero")
    );
}

/// Phase 5: `trait Ordered<T: Equal>` stores Equal as a superclass.
#[test]
fn typeclass_param_bounds_become_superclasses() {
    let src = r#"
            trait Equal<T> { fn eq_val(T a, T b) -> bool; }
            trait Ordered<T: Equal> { fn lt_val(T a, T b) -> bool; }
        "#;
    let (c, msgs) = check_warn(src);
    assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    let ordered = c.generics().typeclass("Ordered").expect("Ordered");
    assert_eq!(ordered.superclasses, vec!["Equal".to_string()]);
    assert!(ordered.has_superclass("Equal", c.generics()));
}

/// Phase 5: `T: Ordered` implies `Equal` ,  `eq_val` resolves without
/// writing `T: Ordered + Equal`.
#[test]
fn implied_superclass_bound_allows_superclass_method() {
    let src = r#"
            trait Equal<T> { fn eq_val(T a, T b) -> bool; }
            trait Ordered<T: Equal> { fn lt_val(T a, T b) -> bool; }
            impl Equal for int {
                pub fn eq_val(int a, int b) -> bool { return a == b; }
            }
            impl Ordered for int {
                pub fn lt_val(int a, int b) -> bool { return a < b; }
            }
            fn cmp_eq<T: Ordered>(T a, T b) -> bool {
                return eq_val(a, b);
            }
            fn main() {
                let x = cmp_eq(1, 1);
            }
        "#;
    let (c, msgs) = check_warn(src);
    assert!(
        msgs.is_empty(),
        "implied Equal under Ordered should typecheck; got: {:?}",
        msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
    );
    // eq_val under Ordered uses flattened slot 1 (after lt_val at 0).
    let hint = c
        .id_table()
        .ids()
        .iter()
        .find_map(|id| c.bound_method_call_at(*id))
        .expect("expected a bound method call for eq_val");
    assert_eq!(hint.method_slot, 1, "eq_val should be superclass slot 1");
    assert_eq!(hint.dict_index, 0);
}

/// Phase 5: `c: * -> Constraint, T: c` can select a concrete subclass
/// and then use its superclass methods through the flattened dictionary.
#[test]
fn abstract_constraint_kind_uses_superclass_method_after_binding() {
    let src = r#"
            trait Equal<T> { fn eq_val(T a, T b) -> bool; }
            trait Ordered<T: Equal> { fn lt_val(T a, T b) -> bool; }
            impl Equal for int {
                pub fn eq_val(int a, int b) -> bool { return a == b; }
            }
            impl Ordered for int {
                pub fn lt_val(int a, int b) -> bool { return a < b; }
            }
            fn choose<c: * -> Constraint, T: c>(T a, T b) -> int {
                if lt_val(a, b) { return 0; }
                if eq_val(a, b) { return 42; }
                return 1;
            }
            fn main() {
                let x = choose(7, 7);
            }
        "#;
    let (c, msgs) = check_warn(src);
    assert!(
        msgs.is_empty(),
        "abstract constraint should bind to Ordered and imply Equal; got: {:?}",
        msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
    );
    let scheme = c.env().lookup("choose").expect("choose scheme");
    assert_eq!(scheme.constraints.len(), 1);
    assert_eq!(scheme.constraints[0].class, "Ordered");

    let slots: Vec<_> = c
        .id_table()
        .ids()
        .iter()
        .filter_map(|id| c.bound_method_call_at(*id).map(|hint| hint.method_slot))
        .collect();
    assert!(
        slots.contains(&0) && slots.contains(&1),
        "expected Ordered slot 0 and implied Equal slot 1, got {:?}",
        slots
    );
}

#[test]
fn generic_call_records_concrete_instance_dict() {
    let src = r#"
            fn add<T: Num>(T a, T b) -> T { return a + b; }
            fn main() { let x = add(1, 2); }
        "#;
    let (mut c, _) = check(src);
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "unexpected: {:?}", msgs);

    let dicts: Vec<_> = c
        .id_table()
        .ids()
        .iter()
        .filter_map(|id| c.call_dicts_at(*id))
        .collect();
    assert_eq!(dicts.len(), 1, "expected one constrained call dict");
    assert_eq!(dicts[0].len(), 1, "expected one Num dictionary");
    assert_eq!(dicts[0][0].class, "Num");
    assert_eq!(dicts[0][0].args, vec![int()]);
}

/// `T: Num` still lowers `a + b` through the flattened Add slot (0).
#[test]
fn num_bound_plus_uses_add_superclass_dict_slot() {
    let src = r#"
            fn add<T: Num>(T a, T b) -> T { return a + b; }
            fn main() { let x = add(1, 2); }
        "#;
    let (c, msgs) = check_warn(src);
    assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    let hint = c
        .id_table()
        .ids()
        .iter()
        .find_map(|id| c.bound_operator_call_at(*id))
        .expect("expected a bound operator call for `+`");
    assert_eq!(hint.dict_index, 0);
    assert_eq!(
        hint.method_slot, 0,
        "Add::add should be slot 0 in Num's flattened dict"
    );
}

/// `T: Add` is enough for `+` without a full `Num` bound.
#[test]
fn add_bound_alone_allows_plus() {
    let src = r#"
            fn just_add<T: Add>(T a, T b) -> T { return a + b; }
            fn main() { let x = just_add(1, 2); }
        "#;
    let (c, msgs) = check_warn(src);
    assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    let hint = c
        .id_table()
        .ids()
        .iter()
        .find_map(|id| c.bound_operator_call_at(*id))
        .expect("expected a bound operator call for `+`");
    assert_eq!(hint.method_slot, 0);
}

/// `T: Ord` still lowers `a < b` through the flattened Lt slot (0).
#[test]
fn ord_bound_lt_uses_lt_superclass_dict_slot() {
    let src = r#"
            fn less<T: Ord>(T a, T b) -> bool { return a < b; }
            fn main() { let x = less(1, 2); }
        "#;
    let (c, msgs) = check_warn(src);
    assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    let hint = c
        .id_table()
        .ids()
        .iter()
        .find_map(|id| c.bound_operator_call_at(*id))
        .expect("expected a bound operator call for `<`");
    assert_eq!(hint.dict_index, 0);
    assert_eq!(
        hint.method_slot, 0,
        "Lt::lt should be slot 0 in Ord's flattened dict"
    );
}

/// `T: Lt` is enough for `<` without a full `Ord` bound.
#[test]
fn lt_bound_alone_allows_less_than() {
    let src = r#"
            fn just_lt<T: Lt>(T a, T b) -> bool { return a < b; }
            fn main() { let x = just_lt(1, 2); }
        "#;
    let (c, msgs) = check_warn(src);
    assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    let hint = c
        .id_table()
        .ids()
        .iter()
        .find_map(|id| c.bound_operator_call_at(*id))
        .expect("expected a bound operator call for `<`");
    assert_eq!(hint.method_slot, 0);
}

//
// The `field_type_for` helper is the codegen-side complement
// to `field_index_for`. It's queried by `receiver_type` when
// resolving chained accesses (`p.x.v`). The helper reads from
// the same `enum_payloads` registry that `field_index_for`
// reads from ,  so the tests below verify the data plumbing,
// not the HM inference logic itself (that's already covered
// by `access_field_*` tests above).

/// `field_type_for` returns the declared type of a record
/// field. Setup: `enum Inner { Inner { v: int } }`. The
/// helper should resolve `"v"` to `int()`.
#[test]
fn field_type_for_returns_record_field_type() {
    let src = "enum Inner { Inner { v: int } }";
    let (c, _) = check(src);
    assert_eq!(
        c.field_type_for("Inner", "v"),
        Some(int()),
        "expected field 'v' on Inner to resolve to int()"
    );
}

/// `field_type_for` returns `None` when the field name isn't
/// declared by any record-shaped variant in the enum. Setup:
/// `enum Inner { Inner { v: int } }`. Asking for `"missing"`
/// should yield `None` ,  the codegen's defensive `LoadField(0)`
/// fallback handles this case.
#[test]
fn field_type_for_returns_none_for_unknown_field() {
    let src = "enum Inner { Inner { v: int } }";
    let (c, _) = check(src);
    assert_eq!(
        c.field_type_for("Inner", "missing"),
        None,
        "expected field 'missing' on Inner to resolve to None"
    );
}

/// `field_type_for` returns `None` when the enum name isn't
/// registered at all. This is the "type error already emitted
/// upstream" case ,  the codegen falls back to `LoadField(0)`.
#[test]
fn field_type_for_returns_none_for_unknown_enum() {
    let (c, _) = check("enum Inner { Inner { v: int } }");
    assert_eq!(
        c.field_type_for("Missing", "v"),
        None,
        "expected field lookup on unregistered enum to resolve to None"
    );
}

/// `field_type_for` returns the correct type for each named
/// field in a record with multiple fields. The test pins the
/// helper's return value to the DECLARED type of each field
/// (not just "any non-None"), so a future refactor that swaps
/// types by mistake would be caught.
#[test]
fn field_type_for_returns_correct_types_for_each_field() {
    let src = "enum Point { Origin, Point { x: int, y: int } }";
    let (c, _) = check(src);
    assert_eq!(c.field_type_for("Point", "x"), Some(int()));
    assert_eq!(c.field_type_for("Point", "y"), Some(int()));
}

/// Synthetic tuple indices (`"0"`, `"1"`, …) resolve via
/// `field_type_for` so derive / Access can `LoadField` tuple
/// payloads without match binders (which clobber instance-method
/// `__dictN` slots).
#[test]
fn field_type_for_returns_tuple_index_types() {
    let src = "enum T { Wrap(int, string) }";
    let (c, _) = check(src);
    assert_eq!(c.field_type_for("T", "0"), Some(int()));
    assert_eq!(c.field_type_for("T", "1"), Some(string()));
    assert_eq!(c.field_type_for("T", "2"), None, "out-of-range tuple index");
}

/// Chained access: field type can be another enum (`Outer.x` → `Inner`).
#[test]
fn field_type_for_returns_enum_type_for_nested_field() {
    let src = "enum Inner { Inner { v: int } } \
                   enum Outer { Outer { x: Inner, y: int } }";
    let (c, _) = check(src);
    // The exact `Ty` shape depends on the typechecker's
    // enum resolution (it could be `Ty::Con("Inner")` or
    // `Ty::Sum { name: "Inner", .. }`). The codegen's
    // `extract_enum_name` handles both shapes via
    // `extract_enum_name(&t).map(|_| t)`. We don't pin the
    // exact Ty here ,  we just verify the helper returns
    // *something* (not `None`) and that it's an enum
    // reference. Use `extract_enum_name` from the codegen
    // crate's perspective: the name should be "Inner".
    let result = c.field_type_for("Outer", "x");
    assert!(
        result.is_some(),
        "expected field 'x' on Outer to resolve to an enum type"
    );
    // Verify the type can be unwrapped to "Inner" via the
    // same logic `enum_name_for_receiver` uses.
    let result_ty = result.unwrap();
    match &result_ty {
        Ty::Con(name) => assert_eq!(name, "Inner"),
        Ty::Sum { name, .. } => assert_eq!(name, "Inner"),
        other => panic!(
            "expected Ty::Con(\"Inner\") or Ty::Sum {{ name: \"Inner\", .. }}, got {:?}",
            other
        ),
    }
}

#[test]
fn range_generic_ord_bound_constructs_range_t() {
    let src = r#"
fn span<T: Ord>(T a, T b) -> Range<T> {
    return a..b;
}
fn main() {
    let r = span(0, 5);
}
"#;
    let (c, _) = check(src);
    assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
    let r_ty = c.codegen_var_type("r").expect("r");
    assert_eq!(
        apply_ty_prune(c.subst(), r_ty),
        crate::typechecking::ty::range_ty(int())
    );
}

#[test]
fn range_to_vec_help_mentions_shared_numeric_step_policy() {
    let src = r#"
fn dump<T: Ord>(T a, T b) {
    let _ = (a..b).to_vec();
}
"#;
    let (c, _) = check(src);
    let help = c
        .messages()
        .iter()
        .find(|m| m.message().contains("cannot iterate"))
        .and_then(|m| m.help().as_ref())
        .expect("expected help on Range.to_vec reject");
    assert!(
        help.contains(".to_vec()") && help.contains("no successor protocol"),
        "expected shared for/.to_vec help, got {help:?}"
    );
}

#[test]
fn impl_method_can_call_module_helper_after_impl() {
    let src = r#"
fn helper(int n) -> int { return n + 1; }
class Foo { pub v: int, }
impl Foo {
    pub fn bump(Foo f) -> int { return helper(f.v); }
}
fn main() {
    let f = new Foo(1);
    let x = f.bump();
}
"#;
    let (c, _) = check(src);
    assert!(
        c.messages().is_empty(),
        "unexpected: {:?}",
        c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
    );
}

#[test]
fn impl_method_can_call_module_helper_before_impl() {
    let src = r#"
class Foo { pub v: int, }
impl Foo {
    pub fn bump(Foo f) -> int { return helper(f.v); }
}
fn helper(int n) -> int { return n + 1; }
fn main() {
    let f = new Foo(1);
    let x = f.bump();
}
"#;
    let (c, _) = check(src);
    assert!(
        c.messages().is_empty(),
        "unexpected: {:?}",
        c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
    );
}

#[test]
fn impl_method_forward_nullary_and_inferred_return() {
    let src = r#"
class Foo { pub v: int, }
impl Foo {
    pub fn bump(Foo f) -> int { return one() + helper(f.v); }
}
fn one() { return 1; }
fn helper(int n) { return n; }
fn main() {
    let f = new Foo(1);
    let x = f.bump();
}
"#;
    let (c, _) = check(src);
    assert!(
        c.messages().is_empty(),
        "unexpected: {:?}",
        c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
    );
}

#[test]
fn impl_method_forward_generic_id_helper() {
    let src = r#"
class Foo { pub v: int, }
impl Foo {
    pub fn bump(Foo f) -> int { return id(f.v); }
}
fn id<T>(T x) -> T { return x; }
fn main() {
    let f = new Foo(1);
    let x = f.bump();
}
"#;
    let (c, _) = check(src);
    assert!(
        c.messages().is_empty(),
        "unexpected: {:?}",
        c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
    );
}

#[test]
fn invoke_refines_return_type_from_impl_self_field() {
    let src = r#"
use ffi::{declare, dload, invoke, Error};
use ffi::types::Float;

class Api {
    pub id: int,
}

impl Api {
    pub fn load(int lib) -> Result<(), Error> {
        self.id = declare(lib, "f", (), Float)?;
    }
}

fn read(Api api) -> Result<float, Error> {
    let f: float = invoke(0, api.id, ())?;
    return f;
}

fn main() -> Result<(), Error> {
    let lib = dload("noop")?;
    let api = new Api(0);
    api.load(lib)?;
    let _ = read(api)?;
}
"#;
    let (c, _) = check(src);
    assert!(
        c.messages().is_empty(),
        "unexpected: {:?}",
        c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
    );
}

/// Pre-pass records param flow even when the helper is defined before callers.
#[test]
fn pre_pass_records_param_invoke_flow_before_main_infer() {
    let src = r#"
use ffi::{declare, dload, invoke, Error};
use ffi::types::Float;

fn helper(int id) -> Result<float, Error> {
    let f: float = invoke(0, id, ())?;
    return f;
}

fn main() -> Result<(), Error> {
    let lib = dload("noop")?;
    let fn_id = declare(lib, "f", (), Float)?;
    let _ = helper(fn_id)?;
}
"#;
    let mut c = Checker::new();
    let trimmed = normalize_adjacent_decls(src.trim());
    let owned = format_check_src(trimmed.as_str());
    let ast = Pratt::default().parse(owned.as_str()).unwrap();
    crate::typechecking::id::pre_walk(&ast, &mut c.ids);
    c.pre_register_enums(&ast).unwrap();
    c.pre_register_free_functions(&ast);
    c.pre_process_top_level_uses(&ast);
    c.pre_pass_ffi_invoke_param_flow(&ast);
    let meta = c
        .test_ffi_param_invoke_ret("helper::id")
        .expect("pre-pass must record helper::id flow");
    assert_eq!(meta.0, float(), "declare ret Float must flow into param");
    assert!(!meta.1, "non-variadic declare must not set variadic bit");
    assert_eq!(meta.2, 0);
}

/// Debug test: discharge_constraints should populate call_site_dicts for
/// user typeclasses at ground call sites.
#[test]
fn discharge_constraints_populates_call_site_dicts_for_user_typeclass() {
    let src = r#"
trait Describable<T> { fn describe_val(T x) -> int; }
impl Describable for int { pub fn describe_val(int x) -> int { return x; } }
fn show<T: Describable>(T x) -> int { return 0; }
fn main() { show(42); }
"#;
    let (c, _) = check(src);
    let dicts = c.all_call_site_dicts();
    eprintln!("call_site_dicts has {} entries", dicts.len());
    for (id, instances) in dicts {
        eprintln!(
            "  NodeId {:?} -> {:?}",
            id,
            instances.iter().map(|i| &i.class).collect::<Vec<_>>()
        );
    }
    let total_instances: usize = dicts.values().map(|v| v.len()).sum();
    assert!(
        total_instances > 0,
        "expected at least one call_site_dict entry for user typeclass, got 0;\
             \ndicts: {:?}",
        dicts
    );
    // Check that we recorded Describable<int>
    let has_describable = dicts
        .values()
        .any(|instances| instances.iter().any(|i| i.class == "Describable"));
    assert!(
        has_describable,
        "expected Describable in call_site_dicts, got: {:?}",
        dicts
    );
}

/// Multi-param trait + `where` clause discharges at a ground call site.
#[test]
fn multiparam_where_clause_discharges_at_call_site() {
    let src = r#"
trait Convert<A, B> { fn cast(A x) -> B; }
impl Convert<int> for int { pub fn cast(int x) -> int { return x; } }
fn apply_cast<A, B>(A x) -> B where Convert<A, B> { return cast(x); }
fn main() { let y = apply_cast(42); }
"#;
    let (mut c, _) = check(src);
    let msgs = c.take_messages();
    assert!(
        msgs.is_empty(),
        "unexpected diagnostics: {:?}",
        msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
    );
    let dicts = c.all_call_site_dicts();
    let has_convert = dicts.values().any(|instances| {
        instances
            .iter()
            .any(|i| i.class == "Convert" && i.args == vec![int(), int()])
    });
    assert!(
        has_convert,
        "expected Convert<int, int> in call_site_dicts, got: {:?}",
        dicts
    );
}

/// Prelude `Into` is registered as a multi-param typeclass.
#[test]
fn prelude_into_trait_is_registered() {
    let g = Generics::new();
    assert!(g.typeclass("From").is_none(), "From must not be registered");
    let into = g.typeclass("Into").expect("Into");
    assert_eq!(into.type_params, vec!["Self".to_string(), "T".to_string()]);
    assert_eq!(into.methods.len(), 1);
    assert_eq!(into.methods[0].name, "into");
}

/// `into` method scheme exists after `check_program`.
#[test]
fn prelude_into_method_scheme_registered() {
    let (c, _) = check("fn main() {}");
    assert!(
        c.typeclass_method_scheme("From", "from").is_none(),
        "From::from must not be registered"
    );
    let into_scheme = c
        .typeclass_method_scheme("Into", "into")
        .expect("Into::into scheme");
    assert_eq!(into_scheme.constraints.len(), 1);
    assert_eq!(into_scheme.constraints[0].class, "Into");
    assert_eq!(into_scheme.constraints[0].args.len(), 2);
}

/// `impl Into<B> for A` with two local classes discharges via `x.into()`.
#[test]
fn prelude_into_method_call_with_expected_type_discharges() {
    let src = r#"
class Celsius { pub c: int }
class Fahrenheit { pub f: int }
impl Into<Fahrenheit> for Celsius {
    fn into(Celsius x) -> Fahrenheit { return new Fahrenheit(x.c); }
}
fn main() {
    let c = new Celsius(0);
    let y: Fahrenheit = c.into();
}
"#;
    let (mut c, _) = check(src);
    let msgs = c.take_messages();
    assert!(
        msgs.is_empty(),
        "unexpected diagnostics: {:?}",
        msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
    );
    let dicts = c.all_call_site_dicts();
    let has_into = dicts.values().any(|instances| {
        instances.iter().any(|i| {
            i.class == "Into"
                && i.args.len() == 2
                && matches!(&i.args[0], Ty::Con(n) if n == "Celsius")
                && matches!(&i.args[1], Ty::Con(n) if n == "Fahrenheit")
        })
    });
    assert!(
        has_into,
        "expected Into<Celsius, Fahrenheit> in call_site_dicts, got: {:?}",
        dicts
    );
}

/// `return c.into();` under `-> Fahrenheit` pins Into's target
/// (Bugbot: expected type must flow from return annotations).
#[test]
fn prelude_into_return_pins_expected_target() {
    let src = r#"
class Celsius { pub c: int }
class Fahrenheit { pub f: int }
class Kelvin { pub k: int }
impl Into<Fahrenheit> for Celsius {
    fn into(Celsius x) -> Fahrenheit { return new Fahrenheit(x.c); }
}
impl Into<Kelvin> for Celsius {
    fn into(Celsius x) -> Kelvin { return new Kelvin(x.c); }
}
fn to_f(Celsius c) -> Fahrenheit {
    return c.into();
}
fn main() {
    let f = to_f(new Celsius(0));
}
"#;
    let (mut c, _) = check(src);
    let msgs = c.take_messages();
    assert!(
        msgs.is_empty(),
        "unexpected diagnostics: {:?}",
        msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
    );
    let dicts = c.all_call_site_dicts();
    let has_f = dicts.values().any(|instances| {
        instances
            .iter()
            .any(|i| i.class == "Into" && matches!(&i.args[1], Ty::Con(n) if n == "Fahrenheit"))
    });
    assert!(
        has_f,
        "expected Into<..., Fahrenheit> from return pin, got: {:?}",
        dicts
    );
}

/// Binder `T: Num` still desugars to a unary Constraint.
#[test]
fn binder_bound_desugars_to_unary_constraint() {
    let src = r#"
fn add<T: Num>(T a, T b) -> T { return a + b; }
"#;
    let (c, _) = check(src);
    let scheme = c.env().lookup("add").expect("add scheme");
    assert_eq!(scheme.constraints.len(), 1);
    assert_eq!(scheme.constraints[0].class, "Num");
    assert_eq!(scheme.constraints[0].args.len(), 1);
    assert!(matches!(
        scheme.constraints[0].args[0],
        Ty::Var(v) if scheme.bounds.contains(&v)
    ));
}

/// Phase 6: open `T::Elem` under `T: Collect` uses a fresh var (not an error).
#[test]
fn assoc_type_open_projection_under_bound_is_ok() {
    let src = r#"
trait Collect<C> {
    type Elem;
    fn head(C xs) -> Elem;
}
fn peek<T: Collect>(T xs) -> T::Elem {
    return head(xs);
}
"#;
    let (mut c, _) = check(src);
    let msgs = c.take_messages();
    assert!(
        msgs.iter().all(|m| !m.message().contains("Cannot find")
            && !m.message().contains("Unknown associated")
            && !m.message().contains("Cannot resolve type projection")),
        "open projection should resolve under Collect bound: {:?}",
        msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
    );
    let scheme = c.env().lookup("peek").expect("peek scheme");
    assert_eq!(scheme.constraints.len(), 1);
    assert_eq!(scheme.constraints[0].class, "Collect");
}

#[test]
fn gat_decl_records_params_and_kind() {
    let src = r#"
trait Pointer<P: * -> *> {
    type Ref<T>;
    fn deref<T>(P<T> ptr) -> Ref<T>;
}
"#;
    let (mut c, _) = check(src);
    let msgs = c.take_messages();
    assert!(
        msgs.is_empty(),
        "unexpected diagnostics: {:?}",
        msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
    );
    let class = c.generics().typeclass("Pointer").expect("Pointer class");
    let assoc = class.assoc_type("Ref").expect("Ref assoc type");
    assert_eq!(assoc.params, vec!["T".to_string()]);
    assert_eq!(assoc.param_kinds, vec![Kind::Type]);
    assert_eq!(assoc.kind, Kind::arrow(Kind::Type, Kind::Type));
}

#[test]
fn gat_method_scheme_quantifies_applied_projection() {
    let src = r#"
trait Pointer<P: * -> *> {
    type Ref<T>;
    fn deref<T>(P<T> ptr) -> Ref<T>;
}
"#;
    let (mut c, _) = check(src);
    let msgs = c.take_messages();
    assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    let scheme = c
        .typeclass_method_schemes
        .get(&("Pointer".to_string(), "deref".to_string()))
        .expect("deref scheme");
    assert_eq!(scheme.assoc_projections.len(), 1);
    assert_eq!(scheme.assoc_projections[0].name, "Ref");
    assert_eq!(scheme.assoc_projections[0].args.len(), 1);
    assert!(
        scheme.bounds.contains(&scheme.assoc_projections[0].var),
        "projection variable must be quantified by the method scheme"
    );
}

#[test]
fn test_case_accepts_assert_in_result_mode() {
    let (mut c, _) = check(
        r#"
test("ok") {
    if !(true) { raise "assertion failed"; }
}
"#,
    );
    let msgs = c.take_messages();
    assert!(
        msgs.is_empty(),
        "unexpected: {:?}",
        msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
    );
    assert_eq!(c.test_case_names(), &["ok".to_string()]);
    assert!(c.fn_is_result_mode("__zs_test_0"));
}

#[test]
fn test_case_registers_multiple_names_in_source_order() {
    let (mut c, _) = check(
        r#"
test("first") { if !(true) { raise "assertion failed"; } }
test("second") { if !(1 == 1) { raise "assertion failed"; } }
"#,
    );
    let msgs = c.take_messages();
    assert!(
        msgs.is_empty(),
        "unexpected: {:?}",
        msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
    );
    assert_eq!(
        c.test_case_names(),
        &["first".to_string(), "second".to_string()]
    );
    assert!(c.fn_is_result_mode("__zs_test_0"));
    assert!(c.fn_is_result_mode("__zs_test_1"));
}

#[test]
fn overload_option_returns_keep_distinct_def_ids() {
    let (c, _) = check(
        r#"
class HeapItem { pub n: int, }
fn f() -> Option<int> { return None; }
fn f(int x) -> Option<HeapItem> { return None; }
fn main() { }
"#,
    );
    let d0 = c.interned_overload_def("f", 0).expect("candidate 0 DefId");
    let d1 = c.interned_overload_def("f", 1).expect("candidate 1 DefId");
    assert_ne!(d0, d1, "overloads must not share a DefId");
}

/// `select_overload` helper: exact fixed match wins over rest.
#[test]
fn select_overload_exact_fixed_beats_rest() {
    let (c, _) = check(
        r#"
fn f(int x) -> int { return x; }
fn f(int x, int y, int... xs) -> int { return x; }
fn main() {}
"#,
    );
    let selected = c.select_overload("f", 1);
    assert!(selected.is_some(), "no overload selected");
    let sel = selected.unwrap();
    assert!(!sel.is_rest, "should select the fixed arity-1 overload");
    assert_eq!(sel.fixed_arity, 1);
}

/// `select_overload` helper: falls back to rest when no exact fixed match.
#[test]
fn select_overload_falls_back_to_rest() {
    let (c, _) = check(
        r#"
fn f(int x) -> int { return x; }
fn f(int x, int y, int... xs) -> int { return x; }
fn main() {}
"#,
    );
    let selected = c.select_overload("f", 5);
    assert!(selected.is_some(), "no overload selected for 5 args");
    let sel = selected.unwrap();
    assert!(sel.is_rest, "should select the rest overload for 5 args");
}

/// Named under-apply records a partial fill mask (bit 0 for `a:`).
#[test]
fn named_partial_records_fill_mask() {
    let (c, _) = check(
        r#"
fn add(int a, int b) -> int { return a + b; }
fn main() { let g = add(a: 1); }
"#,
    );
    assert!(
        !c.partial_fills_by_span.is_empty(),
        "expected partial_fills_by_span entry for named under-apply"
    );
    assert!(
        c.partial_fills_by_span.values().any(|&mask| mask == 0b01),
        "expected fill mask 0b01 for `a:`; got {:?}",
        c.partial_fills_by_span.values().collect::<Vec<_>>()
    );
}

#[test]
fn is_thread_sendable_ty_accepts_immediates_strings_and_host_handles() {
    use crate::typechecking::ty::{mutex_ty, receiver_ty, rwlock_ty, sender_ty};
    let c = Checker::new();
    assert!(c.is_thread_sendable_ty(&int()));
    assert!(c.is_thread_sendable_ty(&string()));
    assert!(c.is_thread_sendable_ty(&boolean()));
    assert!(c.is_thread_sendable_ty(&unit_ty()));
    assert!(c.is_thread_sendable_ty(&sender_ty()));
    assert!(c.is_thread_sendable_ty(&receiver_ty()));
    assert!(c.is_thread_sendable_ty(&mutex_ty()));
    assert!(c.is_thread_sendable_ty(&rwlock_ty()));
    assert!(c.is_thread_sendable_ty(&array(int())));
    assert!(c.is_thread_sendable_ty(&tuple_ty(vec![int(), string()])));
    assert!(c.is_thread_sendable_ty(&tuple_ty(vec![receiver_ty(), sender_ty()])));
}

#[test]
fn is_thread_sendable_ty_rejects_stream_coroutine_and_functions() {
    use crate::typechecking::ty::stream_ty;
    let c = Checker::new();
    assert!(!c.is_thread_sendable_ty(&stream_ty()));
    assert!(!c.is_thread_sendable_ty(&crate::typechecking::ty::thread_ty()));
    assert!(!c.is_thread_sendable_ty(&Ty::Fun(Box::new(unit_ty()), Box::new(int()))));
    assert!(!c.is_thread_sendable_ty(&Ty::App(
        Box::new(Ty::Con("coroutine".into())),
        vec![int(), unit_ty()]
    )));
    assert!(!c.is_thread_sendable_ty(&array(stream_ty())));
    assert!(!c.is_thread_sendable_ty(&tuple_ty(vec![int(), stream_ty()])));
}

#[test]
fn host_fn_scheme_covers_all_wiring_registries() {
    use machine::{CLOCK_WIRING, ENV_WIRING, FS_WIRING};

    let mut c = Checker::new();
    let names: Vec<&str> = FS_WIRING
        .iter()
        .chain(ENV_WIRING.iter())
        .chain(CLOCK_WIRING.iter())
        .map(|&(n, _, _)| n)
        .collect();
    assert_eq!(names.len(), 27);
    for name in names {
        let _ = c.host_fn_scheme(name, 0..0);
    }
    let msgs = c.take_messages();
    assert!(
        msgs.is_empty(),
        "wired registries must not hit host_fn_scheme fallback: {:?}",
        msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
    );
}

#[test]
fn host_fn_scheme_unknown_registry_emits_diagnostic() {
    let mut c = Checker::new();
    let _ = c.host_fn_scheme("no_such_native", 0..0);
    let msgs = c.take_messages();
    assert_eq!(msgs.len(), 1);
    assert!(
        msgs[0]
            .message()
            .contains("unknown host native `no_such_native`"),
        "got: {}",
        msgs[0].message()
    );
}
#[test]
fn module_qualified_type_overloads_resolve_bare_calls() {
    // Namespaced registration + within-module bare calls (as in `stdlib/num.hy`).
    let mut c = Checker::new();
    c.set_current_module("num");
    let src = r#"
fn min(int a, int b) -> int { return a; }
fn min(float a, float b) -> float { return a; }
fn max(int a, int b) -> int { return a; }
fn max(float a, float b) -> float { return a; }
fn clamp(int x, int lo, int hi) -> int { return min(max(x, lo), hi); }
fn clamp(float x, float lo, float hi) -> float { return min(max(x, lo), hi); }
"#;
    let parser = Pratt::default();
    let ast = parser.parse(src).expect("parse");
    let _ = c.check_program(&ast);
    let msgs = c.take_messages();
    assert!(
        msgs.is_empty(),
        "unexpected diagnostics: {:?}",
        msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
    );
    assert_eq!(
        c.overload_candidates("min").map(|v| v.len()),
        Some(2),
        "bare lookup should see num::min via current_module"
    );
    assert_eq!(c.overload_candidates("num::min").map(|v| v.len()), Some(2));
    assert_eq!(c.overload_candidates("clamp").map(|v| v.len()), Some(2));
}

#[test]
fn module_enum_fqn_retained_across_check_program() {
    let mut c = Checker::new();
    let parser = Pratt::default();

    c.set_current_module("pkg");
    let ast = parser
        .parse("enum Traffic { Go, Stop, }")
        .expect("parse pkg");
    let _ = c.check_program(&ast);
    assert!(
        c.take_messages().is_empty(),
        "pkg: {:?}",
        c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
    );
    assert_eq!(c.tag_for("pkg::Traffic", "Go"), Some(0));
    assert_eq!(c.tag_for("pkg::Traffic", "Stop"), Some(1));

    c.set_current_module("");
    let ast = parser
        .parse("enum Local { A, B, }")
        .expect("parse root enum");
    let _ = c.check_program(&ast);
    assert!(
        c.take_messages().is_empty(),
        "root: {:?}",
        c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
    );
    assert_eq!(
        c.tag_for("pkg::Traffic", "Go"),
        Some(0),
        "module FQN tags must survive the next check_program"
    );
    assert_eq!(c.tag_for("Local", "A"), Some(0));

    let ast = parser.parse("fn main() {}").expect("parse next file");
    let _ = c.check_program(&ast);
    assert!(c.take_messages().is_empty());
    assert_eq!(
        c.tag_for("pkg::Traffic", "Go"),
        Some(0),
        "module FQN tags must survive a third check_program"
    );
    assert!(
        c.tag_for("Local", "A").is_none(),
        "bare entry-module enum tags must be cleared between programs"
    );
}

#[test]
fn diagnostic_ranges_valid_for_bare_expr_type_errors() {
    for src in &["1 + true", "true + 1", r#""hi" + 1"#] {
        let (mut c, _) = check(src);
        let src_len = src.len();
        for msg in c.take_messages() {
            let r = msg.range();
            assert!(
                r.start <= r.end && r.end <= src_len,
                "wrapped probe must not skew spans: {:?} for len {} (msg: {})",
                r,
                src_len,
                msg.message()
            );
        }
    }
}

#[test]
fn inferred_expr_ty_reads_trailing_program_expression() {
    assert_eq!(
        inferred_expr_ty("let x: int = 1; let y: int = 2; x + y"),
        int()
    );
    assert_eq!(
        inferred_expr_ty(r#"class Foo { pub v: int } let x = new Foo(7); x.v"#),
        int()
    );
}

#[test]
fn adjacent_enum_and_let_declarations_parse() {
    let src = "enum E { A, B } let x = E::A; x";
    let (mut c, _) = check(src);
    assert!(c.take_messages().is_empty(), "{:?}", c.take_messages());
    let ty = inferred_expr_ty(src);
    assert_ne!(ty, unit_ty(), "trailing enum binding should not be unit");
    let scheme = c.env().lookup("x").expect("x bound");
    let mut counter = TyVarCounter::new();
    let bound = apply_ty_prune(c.subst(), &instantiate(scheme, &mut counter));
    assert_eq!(ty, bound, "trailing expr type should match binding");
}

#[test]
fn block_trailing_expr_without_semi_is_block_value() {
    assert_eq!(inferred_expr_ty("{ 1; 2; 3 }"), int());
    assert_eq!(inferred_expr_ty("{ { 99 } }"), int());
    // Contrast: a trailing `expr;` inside a block is a statement (unit).
    let (_, ty) = check("fn f() { 42; }");
    assert_eq!(ty, unit_ty());
}

#[test]
fn expr_statement_at_program_level_is_unit() {
    let (_, ty) = check("1 + 2;");
    assert_eq!(ty, unit_ty());
}

#[test]
fn bare_expr_probe_does_not_break_error_diagnostic_count() {
    let msgs_stmt = assert_messages("1 + true;");
    let msgs_bare = assert_messages("1 + true");
    assert!(!msgs_stmt.is_empty());
    assert!(!msgs_bare.is_empty());
    assert_eq!(msgs_stmt.len(), msgs_bare.len());
}

/// Macro output writes `impl m::Trait for T` (hygiene); source written by
/// hand is rejected before checking (`attrs::expand_source_in`).
#[test]
fn impl_head_accepts_module_qualified_trait_name() {
    let mut c = Checker::new();
    let parser = Pratt::default();

    c.set_current_module("rank");
    let ast = parser
        .parse("trait Rank<T> { fn rank(T x) -> int {} }")
        .expect("parse rank");
    let _ = c.check_program(&ast);
    assert!(c.take_messages().is_empty());

    c.set_current_module("");
    let importer = r#"
class Card { pub v: int, }
impl rank::Rank for Card {
    pub fn rank(Card c) -> int {
        return c.v;
    }
}
"#;
    let ast = parser.parse(importer).expect("parse importer");
    let _ = c.check_program(&ast);
    let msgs = c.take_messages();
    assert!(
        msgs.is_empty(),
        "qualified trait impl head should resolve: {:?}",
        msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
    );
    assert!(
        c.generics
            .find_instance("Rank", &[Ty::Con("Card".into())])
            .is_some(),
        "expected a Rank instance for Card"
    );
}

#[test]
fn logical_not_on_type_param_help_names_the_param() {
    // coil-lang#1447: the help named the internal variable (`found t61`).
    let src = r#"
fn negate<T>(T x) -> bool {
    return !x;
}
"#;
    let (c, _) = check(src);
    let help = c
        .messages()
        .iter()
        .find(|m| m.message().contains("Logical NOT"))
        .and_then(|m| m.help().as_ref())
        .expect("expected help on `!` reject");
    assert!(help.starts_with("found `T`;"), "got {help:?}");
}

#[test]
fn not_iterable_type_param_message_names_the_param() {
    // coil-lang#1447: diagnostics show a type parameter by its source name,
    // never as the internal variable (`t61`).
    let src = r#"
fn walk<T>(T x) {
    for y in x { }
}
"#;
    let (c, _) = check(src);
    let msg = c
        .messages()
        .iter()
        .find(|m| m.message().contains("is not iterable"))
        .expect("expected a not-iterable reject");
    assert_eq!(msg.message(), "type `T` is not iterable");
}
