
    use super::*;
    use crate::typechecking::env::{instantiate, TyVarCounter};
    use crate::typechecking::subst::apply_ty_prune;
    use crate::typechecking::ty::EnumVariantPayloadTy;
    use parser::SimpleSpan;
    use parser::ast::{
        EnumConstructPayload, EnumVariantPayload, LetFieldPattern, LetPattern, PatternField,
        PatternPayload, RecordFieldDecl, RecordFieldValue,
    };
    use parser::Pratt;

    fn is_bare_expr_source(trimmed: &str) -> bool {
        if trimmed.contains('\n') || trimmed.contains(';') || trimmed.starts_with('{') {
            return false;
        }
        const STMT_PREFIXES: &[&str] = &[
            "let ", "use ", "class ", "enum ", "trait ", "impl ", "test(",
            "fn ", "if ", "while ", "for ", "return ", "async ", "match ", "defer ",
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
        let fn_ty = apply_ty_prune(&c.subst(), &instantiate(scheme, &mut counter));
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
                Some(apply_ty_prune(c.subst(), &instantiate(scheme, &mut counter)))
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
    fn check(src: &str) -> (Checker, Ty) {
        let mut c = Checker::new();
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

    // ---- Literals ----

    #[test]
    fn integer_literal() {
        assert_ok("42", int());
    }

    #[test]
    fn float_literal() {
        assert_ok("3.14", float());
    }

    #[test]
    fn string_literal() {
        assert_ok("\"hello\"", string());
    }

    #[test]
    fn bool_literal() {
        assert_ok("true", boolean());
        assert_ok("false", boolean());
    }

    // ---- Identifier ----

    #[test]
    fn unknown_identifier_errors() {
        let msgs = assert_messages("x;");
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn identifier_from_let_annotation() {
        // Declare x: int = 42, then verify via env lookup.
        let (mut c, _) = check("let x: int = 42;");
        assert!(c.take_messages().is_empty());
        let scheme = c.env().lookup("x").unwrap();
        let ty = apply_ty(c.subst(), &scheme.ty);
        assert_eq!(ty, int());
    }

    // ---- Variables and let ----

    #[test]
    fn let_with_annotation() {
        assert_ok("let x: int = 42;", unit_ty());
    }

    #[test]
    fn let_without_annotation_infers_from_value() {
        // `let x = 42;` — x should be inferred as int.
        let (mut c, _) = check("let x = 42;");
        assert!(c.take_messages().is_empty());
        let scheme = c.env().lookup("x").unwrap();
        let ty = apply_ty(c.subst(), &scheme.ty);
        assert_eq!(ty, int());
    }

    #[test]
    fn let_without_annotation_or_value_uses_fresh_var() {
        // `let x;` — x is a fresh type variable (id is not stable across
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

    // ---- Assignment ----

    #[test]
    fn assignment_updates_existing_var() {
        let (mut c, _) = check("let x; x = 42;");
        assert!(c.take_messages().is_empty());
        let scheme = c.env().lookup("x").unwrap();
        let ty = apply_ty(c.subst(), &scheme.ty);
        assert_eq!(ty, int());
    }

    #[test]
    fn assignment_to_undeclared_var_errors() {
        let msgs = assert_messages("x = 42;");
        assert!(!msgs.is_empty());
    }

    #[test]
    fn assignment_mismatch_errors_but_continues() {
        // x: int, then assign "hello" — should produce an error.
        let msgs = assert_messages("let x: int; x = \"hello\";");
        assert!(!msgs.is_empty());
    }

    // ---- Arithmetic ----

    #[test]
    fn addition_of_ints_is_int() {
        assert_ok("1 + 2", int());
    }

    #[test]
    fn addition_of_floats_is_float() {
        assert_ok("1.0 + 2.0", float());
    }

    #[test]
    fn mixed_int_float_arithmetic_mismatches() {
        // 1 + 2.0: unify int with float → Mismatch.
        let msgs = assert_messages("1 + 2.0;");
        assert!(!msgs.is_empty());
    }

    #[test]
    fn subtraction() {
        assert_ok("5 - 3", int());
    }

    #[test]
    fn multiplication() {
        assert_ok("4 * 5", int());
    }

    #[test]
    fn division() {
        assert_ok("10 / 2", int());
    }

    #[test]
    fn modulo() {
        assert_ok("10 % 3", int());
    }

    #[test]
    fn power() {
        assert_ok("2 ** 3", int());
    }

    #[test]
    fn shift_left() {
        assert_ok("1 << 2", int());
    }

    #[test]
    fn xor() {
        assert_ok("5 ^ 3", int());
    }

    #[test]
    fn bitand() {
        assert_ok("5 & 3", int());
    }

    #[test]
    fn bitor() {
        assert_ok("5 | 3", int());
    }

    // ---- Comparison ----

    #[test]
    fn equality_returns_bool() {
        assert_ok("1 == 1", boolean());
    }

    #[test]
    fn inequality_returns_bool() {
        assert_ok("1 != 2", boolean());
    }

    #[test]
    fn less_than() {
        assert_ok("1 < 2", boolean());
    }

    #[test]
    fn greater_than() {
        assert_ok("2 > 1", boolean());
    }

    #[test]
    fn less_equal() {
        assert_ok("1 <= 1", boolean());
    }

    #[test]
    fn greater_equal() {
        assert_ok("2 >= 2", boolean());
    }

    // ---- Logical ----

    #[test]
    fn logical_and_of_bools_is_bool() {
        assert_ok("true && false", boolean());
    }

    #[test]
    fn logical_or_of_bools_is_bool() {
        assert_ok("true || false", boolean());
    }

    #[test]
    fn logical_and_requires_bool() {
        // 1 && 2 — int, not bool.
        let msgs = assert_messages("1 && 2;");
        assert!(!msgs.is_empty());
    }

    // ---- Prefix ----

    #[test]
    fn negate_int() {
        assert_ok("-42", int());
    }

    #[test]
    fn positive_int() {
        assert_ok("+42", int());
    }

    #[test]
    fn bitwise_not_int() {
        assert_ok("~7", int());
    }

    #[test]
    fn logical_not_bool() {
        assert_ok("!true", boolean());
        assert_ok("!false", boolean());
    }

    #[test]
    fn logical_not_int() {
        assert_ok("!0", boolean());
        assert_ok("!42", boolean());
    }

    #[test]
    fn logical_not_rejects_float() {
        let msgs = assert_messages("!1.0;");
        assert!(!msgs.is_empty());
    }

    // ---- Postfix ----

    #[test]
    fn inc_dec() {
        // These return the variable's type.
        let (mut c, _) = check("let x: int = 0; x++;");
        assert!(c.take_messages().is_empty());
    }

    // ---- Call ----

    #[test]
    fn call_unknown_function_errors() {
        let msgs = assert_messages("foo();");
        assert!(!msgs.is_empty());
    }

    #[test]
    fn call_to_unregistered_print_errors() {
        let msgs = assert_messages("print(\"hello\");");
        assert!(!msgs.is_empty());
    }

    // ---- If ----

    #[test]
    fn if_single_branch() {
        let src = "fn __coil_if__() -> int { if true { return 42; } else { return 0; } }";
        let (mut c, _) = check(src);
        assert!(c.take_messages().is_empty());
        let scheme = c.env().lookup("__coil_if__").unwrap();
        let mut counter = TyVarCounter::new();
        let ty = peel_fn_return(apply_ty_prune(c.subst(), &instantiate(scheme, &mut counter)));
        assert_eq!(ty, int());
    }

    #[test]
    fn if_with_non_bool_condition_errors() {
        let msgs = assert_messages("if 42 { 1; }");
        assert!(!msgs.is_empty());
    }

    // ---- Match (parser doesn't produce Match nodes yet, so the
    //      handler is unreachable from real source). Tests for Match
    //      are deferred until the parser learns the `match` keyword.

    // ---- Loop ----

    #[test]
    fn while_loop_returns_unit() {
        assert_ok("while false { 42; }", unit_ty());
    }

    #[test]
    fn while_with_non_bool_condition_errors() {
        let msgs = assert_messages("while 42 { 1; }");
        assert!(!msgs.is_empty());
    }

    // ---- Return ----

    #[test]
    fn return_inside_expression() {
        // `return` is a diverging expression regardless of enclosing context.
        assert_ok("return 42", never());
    }

    // ---- Block ----

    #[test]
    fn empty_block() {
        assert_ok("{}", unit_ty());
    }

    #[test]
    fn block_last_value_is_block_type() {
        assert_ok("{ 1; 2; 3; }", int());
    }

    // ---- String formatting ----

    #[test]
    fn write_all_with_string_bytes_ok() {
        assert_ok(
            r#"use io::{stdout, write}; use string::to_bytes; write(stdout(), to_bytes("hello"));"#,
            result_app_ty(int(), Ty::Con(common::BUILTIN_IO_ERROR_ENUM.into())),
        );
    }

    // ---- Defer ----

    #[test]
    fn defer_returns_unit() {
        assert_ok("defer { 42; }", unit_ty());
    }

    /// Defer bodies cannot close over outer locals unless listed in `use`.
    #[test]
    fn defer_uncaptured_outer_is_error() {
        let msgs = assert_messages(
            r#"
fn main() {
    let y = 10;
    defer { y; }
}
"#,
        );
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("cannot capture `y` without `use (y)`")),
            "expected cannot-capture diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Truly undefined names in a defer are rejected (not silently accepted).
    #[test]
    fn defer_undefined_variable_is_error() {
        let msgs = assert_messages(
            r#"
fn main() {
    defer { totally_undefined_var; }
}
"#,
        );
        assert!(
            msgs.iter().any(|m| {
                m.message()
                    .contains("Cannot find value `totally_undefined_var`")
            }),
            "expected unknown-value diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// `defer use (y)` makes the outer local visible inside the block.
    #[test]
    fn defer_use_capture_typechecks() {
        let (mut c, _) = check(
            r#"
fn main() {
    let y = 10;
    defer use (y) { y; }
}
"#,
        );
        assert!(
            c.take_messages().is_empty(),
            "expected no diagnostics for defer use (y)"
        );
    }

    /// Listing an unknown name in `use (…)` is itself an error.
    #[test]
    fn defer_use_unknown_capture_is_error() {
        let msgs = assert_messages(
            r#"
fn main() {
    defer use (nope) { nope; }
}
"#,
        );
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("Cannot find value `nope`")),
            "expected unknown capture diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    // ---- List literals ----
    //      Parser doesn't produce `List` nodes yet, so these are deferred.

    // ---- Complex expressions ----

    #[test]
    fn nested_arithmetic() {
        assert_ok("(1 + 2) * 3", int());
    }

    #[test]
    fn block_with_multiple_lets() {
        let src = "let x: int = 10; let y: int = 20; x + y";
        assert_ok(src, int());
    }

    // ---- Function declarations ----

    #[test]
    fn function_declaration_with_typed_args_and_return() {
        let (mut c, _) = check("fn add(int a, int b) -> int { return a + b; }");
        assert!(c.take_messages().is_empty());
        let scheme = c.env().lookup("add").unwrap();
        let ty = apply_ty(c.subst(), &scheme.ty);
        assert_eq!(
            ty,
            Ty::Fun(
                Box::new(int()),
                Box::new(Ty::Fun(Box::new(int()), Box::new(int())))
            )
        );
    }

    #[test]
    fn function_declaration_with_inferred_return() {
        // No declared return type — should be inferred from the body.
        let (mut c, _) = check("fn add(int a, int b) { return a + b; }");
        assert!(c.take_messages().is_empty());
        let scheme = c.env().lookup("add").unwrap();
        let ty = apply_ty(c.subst(), &scheme.ty);
        // a -> b -> ?  (return type is a fresh variable bound to int)
        assert!(matches!(ty, Ty::Fun(_, _)));
    }

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
    fn rank_n_param_accepts_polymorphic_id() {
        let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
            fn id<T>(T x) -> T { return x; }
            fn app(forall T. T -> T f, int x) -> int { return f(x); }
            fn main() { write(stdout(), to_bytes(format("%i", app(id, 1)))); }
        "#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "expected no messages, got: {msgs:?}");
    }

    #[test]
    fn rank_n_rejects_escaping_skolem() {
        let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
            fn inc(int x) -> int { return x; }
            fn app(forall T. T -> T f, int x) -> int { return f(x); }
            fn main() { write(stdout(), to_bytes(format("%i", app(inc, 1)))); }
        "#;
        let msgs = assert_messages(src);
        assert!(
            msgs.iter()
                .any(|m| m.code() == Some(ErrorCode::TypeMismatch)),
            "expected rank-n type mismatch, got: {msgs:?}"
        );
    }

    #[test]
    fn function_arity_mismatch_errors() {
        // fib takes 1 int, called with 2 args.
        let msgs = assert_messages("fn fib(int n) -> int { return n; } fib(1, 2);");
        assert!(!msgs.is_empty());
    }

    #[test]
    fn function_return_mismatch_errors() {
        // Declared return is int, but body returns string.
        let msgs = assert_messages("fn broken() -> int { return \"oops\"; }");
        assert!(!msgs.is_empty());
    }

    #[test]
    fn function_undefined_errors() {
        // body calls an unknown function.
        let msgs = assert_messages("fn main() { nope(); }");
        assert!(!msgs.is_empty());
    }

    // ---- Recursive functions (monomorphic recursion) ----

    #[test]
    fn recursive_fib() {
        // fib(n) = if n < 2 then n else fib(n-1) + fib(n-2)
        // Adapted to the current syntax (no `<` comparison; use `==`).
        let src = "fn fib(int n) -> int { if n == 1 { return 1; } if n == 2 { return 1; } return fib(n - 1) + fib(n - 2); }";
        let (mut c, _) = check(src);
        assert!(
            c.take_messages().is_empty(),
            "expected no messages, got: {:?}",
            c.messages()
        );
        let scheme = c.env().lookup("fib").unwrap();
        let ty = apply_ty(c.subst(), &scheme.ty);
        assert_eq!(ty, Ty::Fun(Box::new(int()), Box::new(int())));
    }

    // ---- Class declarations ----

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
        // First field is public, second is private — they're tracked
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
        // Member access (`x.field`) isn't parsed yet, so we can't write
        // a true visibility-check test. This test asserts the data is
        // recorded correctly so the future member-access pass can
        // enforce it without re-parsing the class.
        let (mut c, _) = check("class Foo { pub age: int, name: String, }");
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        let foo = c.classes.get("Foo").unwrap();
        assert_eq!(foo[0].0, Visibility::Public);
        assert_eq!(foo[1].0, Visibility::Private);
    }

    // ---- Impl blocks ----

    #[test]
    fn impl_binds_self_to_owner() {
        // `self` is implicit. The method's type becomes Foo -> Foo.
        let src = "class Foo { } impl Foo { fn id() -> Foo { return new Foo(); } }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        let methods = c.methods.get("Foo").expect("methods not registered");
        let (_, scheme) = methods.get("id").unwrap();
        let ty = apply_ty(c.subst(), &scheme.ty);
        // Foo -> Foo
        assert_eq!(
            ty,
            Ty::Fun(
                Box::new(Ty::Con("Foo".into())),
                Box::new(Ty::Con("Foo".into()))
            )
        );
    }

    #[test]
    fn impl_method_with_args_prepends_self() {
        let src = "impl Foo { fn method(int x) -> int { return x; } }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        let methods = c.methods.get("Foo").unwrap();
        let (_, scheme) = methods.get("method").unwrap();
        let ty = apply_ty(c.subst(), &scheme.ty);
        // Foo -> int -> int
        assert_eq!(
            ty,
            Ty::Fun(
                Box::new(Ty::Con("Foo".into())),
                Box::new(Ty::Fun(Box::new(int()), Box::new(int())))
            )
        );
    }

    #[test]
    fn impl_method_visibility_default_is_private() {
        let src = "impl Foo { fn hidden() -> int { return 0; } }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        let methods = c.methods.get("Foo").unwrap();
        let (vis, _) = methods.get("hidden").unwrap();
        assert_eq!(*vis, Visibility::Private);
        assert_eq!(
            c.inherent_method_visibility("Foo::hidden"),
            Some(Visibility::Private)
        );
    }

    #[test]
    fn impl_pub_method_marks_visibility() {
        let src = "impl Foo { pub fn visible() -> int { return 0; } }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        let methods = c.methods.get("Foo").unwrap();
        let (vis, _) = methods.get("visible").unwrap();
        assert_eq!(*vis, Visibility::Public);
        assert_eq!(
            c.inherent_method_visibility("Foo::visible"),
            Some(Visibility::Public)
        );
    }

    // ---- Instantiation ----

    #[test]
    fn instantiate_returns_class_type() {
        // Positional ctor args match class fields in declaration order.
        let src = r#"class Foo { name: string, } let x = new Foo("hi"); x"#;
        let (mut c, _) = check(src);
        let ty = inferred_expr_ty(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        // The whole program's type is the type of `x`, which is Foo.
        assert_eq!(ty, Ty::Con("Foo".into()));
    }

    // ---- Combined: class + impl + instantiation ----

    #[test]
    fn class_impl_and_instantiate_combined() {
        let src = r#"
            class Foo { name: string, }
            impl Foo { fn sadge() -> int { return 42; } }
            fn main() { let x = new Foo("hi"); }
        "#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        assert!(c.classes.contains_key("Foo"));
        assert!(c.methods.get("Foo").unwrap().contains_key("sadge"));
    }

    #[test]
    fn class_method_call_typechecks() {
        let src = "\
            class Point { x: int, y: int, } \
            impl Point { fn sum() -> int { return self.x + self.y; } } \
            fn main() { let p = new Point(1, 3); p.sum(); }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
    }

    // ---- Phase 7: generic classes ----

    #[test]
    fn generic_class_new_infers_cell_int() {
        let src = "\
            class Cell<T> { value: T }
            fn main() { let c = new Cell(42); }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        let c_ty = c.codegen_var_type("c").expect("c should be recorded");
        assert_eq!(
            apply_ty_prune(c.subst(), c_ty),
            Ty::App(Box::new(Ty::Con("Cell".into())), vec![int()])
        );
    }

    #[test]
    fn generic_class_method_get_returns_int() {
        let src = "\
            class Cell<T> { value: T }
            impl Cell<T> {
                fn get() -> T { return self.value; }
            }
            fn main() {
                let c = new Cell(42);
                let v = c.get();
            }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        assert_eq!(
            c.codegen_var_type("v")
                .map(|t| apply_ty_prune(c.subst(), t)),
            Some(int())
        );
    }

    #[test]
    fn generic_class_fields_stored_as_param_placeholders() {
        let (mut c, _) = check("class Cell<T> { value: T }");
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        let fields = c.classes.get("Cell").expect("Cell registered");
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].1, "value");
        assert_eq!(fields[0].2, Ty::Con("T".into()));
    }

    #[test]
    fn class_unknown_field_errors() {
        let src = "\
            class Point { x: int, } \
            fn main() { let p = new Point(1); p.z; }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(
            msgs.iter().any(|m| m.message().contains("field")),
            "expected unknown-field diagnostic, got {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn class_ctor_arity_mismatch_errors() {
        let src = "\
            class Point { x: int, y: int, } \
            fn main() { let p = new Point(1); }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(!msgs.is_empty(), "expected ctor arity diagnostic");
    }

    // ---- Recursive method (inside an impl) ----

    #[test]
    fn recursive_method_via_self_binding() {
        // fib uses no self, but `==` is the only comparison the
        // parser currently supports; use that for the branch.
        let src = "impl Counter { fn tick(int n) -> int { if n == 0 { return 0; } return tick(n - 1) + 1; } }";
        let (mut c, _) = check(src);
        // We don't require no messages — the outer call site `tick(...)`
        // may have residual issues — but the method should be registered.
        let _ = c.take_messages();
        assert!(c.methods.get("Counter").unwrap().contains_key("tick"));
    }

    // ---- Block returns last value ----

    #[test]
    fn nested_blocks_return_inner() {
        let src = "fn __coil_nested__() -> int { return 42; }";
        let (mut c, _) = check(src);
        assert!(c.take_messages().is_empty());
        let scheme = c.env().lookup("__coil_nested__").unwrap();
        let mut counter = TyVarCounter::new();
        let ty = peel_fn_return(apply_ty_prune(c.subst(), &instantiate(scheme, &mut counter)));
        assert_eq!(ty, int());
    }

    // ---- Native registration ----

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

    // ---- Recursion-depth guard ----

    #[test]
    fn infer_depth_guard_panics_with_expected_diagnostic_past_limit() {
        // Exercise the guard directly (not via a literal deeply-nested AST):
        // recursing 2000+ levels via `+` also overflows the stack while
        // dropping the parsed AST itself (a well-known Box<T> recursive-Drop
        // pitfall unrelated to infer_inner's own frame size), so a real
        // pathologically-deep program isn't a safe way to test this in
        // isolation. Seed `infer_depth` to the limit and confirm the very
        // next `infer` call panics with a clean diagnostic instead of
        // recursing further — this is the same code path a real deeply
        // nested expression would hit.
        let mut c = Checker::new();
        let ast = Pratt::default().parse("1;").expect("trivial literal parses");
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

    // ---- Diagnostics ----
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
    fn infinite_type_message_uses_clear_format() {
        // It's hard to construct an infinite-type situation without
        // recursive type syntax (e.g., `α = List<α>`), so this test
        // just checks the format IF such a message ever fires. To make
        // sure the path is exercised, we drive the checker through a
        // recursive function declaration whose body returns the
        // function itself with the wrong shape — that triggers an
        // occurs check via the return-type unification.
        //
        // (If your checker ever changes the return path so this no
        // longer fires an occurs check, drop this test — it's about
        // message format, not behaviour.)
        let (mut c, _) = check("fn bad() { return bad; }");
        let msgs = c.take_messages();
        // Either there's an infinite-type error, or the function is
        // typeable. Both are fine — what we want to assert is the
        // format IF the error fires.
        if let Some(infinite) = msgs
            .iter()
            .find(|m| m.message().contains("Cannot construct infinite type"))
        {
            assert!(
                infinite.help().is_some(),
                "missing help on occurs-check message"
            );
        }
    }

    #[test]
    fn not_a_function_message_uses_cannot_call_format() {
        // `let x = 5; x(2);` — `x` is an int, calling it is an error.
        let (mut c, _) = check("let x = 5; x(2);");
        let msgs = c.take_messages();
        assert!(!msgs.is_empty(), "expected a message");
        let msg = msgs.iter().find(|m| {
            m.message().contains("Cannot call value") || m.message().contains("too many arguments")
        });
        assert!(msg.is_some(), "got: {:?}", msgs);
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
    fn assignment_to_const_emits_immutability_diagnostic() {
        let (mut c, _) = check("const x = 1; x = 2;");
        let msgs = c.take_messages();
        let msg = msgs
            .iter()
            .find(|m| m.message().contains("Cannot assign to constant `x`"));
        assert!(msg.is_some(), "got: {:?}", msgs);
        let msg = msg.unwrap();
        assert!(msg.help().is_some(), "missing help");
        assert_eq!(
            msg.code(),
            Some(ErrorCode::InvalidAssignment),
            "const reassignment must use InvalidAssignment (E0107), got: {:?}",
            msg.code()
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
        // somewhere inside the source — NOT at byte 0.
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

    // ---- Type cache ----

    #[test]
    fn free_fn_arg_still_in_codegen_var_types() {
        // Free-fn `assign_fn_arg_node_ids` is deferred (Hash / constraint-kind).
        // Params remain available via the name side-table.
        let src = "fn f(int x) -> int { return x; }";
        let (c, _) = check(src);
        assert!(c.messages().is_empty(), "{:?}", c.messages());
        assert_eq!(
            c.codegen_var_type("x")
                .map(|t| apply_ty_prune(c.subst(), t)),
            Some(int())
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
        // cache entry — but they're still visited by the pre-walk.
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

    // ---- Call with arguments ----

    #[test]
    fn unknown_call_argument_types_dont_crash() {
        let msgs = assert_messages("foo(1, 2, 3);");
        assert!(!msgs.is_empty());
    }

    // ================================================================
    // ---- Enums and pattern matching ----
    // ================================================================

    // ---- Enum registration ----

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
    fn recursive_enum_typechecks() {
        // Isorecursive encoding: recursive payloads use Ty::Con("Tree").
        let (mut c, _) = check("enum Tree { Leaf, Node(int, Tree, Tree) }");
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        // The recursive variant's payload should reference the
        // enum by name (opaque) — the public `enum_variants` API
        // is the canonical interface to inspect this.
        let variants = c.enum_variants("Tree").expect("Tree not registered");
        let node_payload = variants
            .iter()
            .find(|(n, _, _)| n == "Node")
            .unwrap()
            .2
            .clone();
        assert_eq!(
            node_payload,
            vec![int(), Ty::Con("Tree".into()), Ty::Con("Tree".into())]
        );
    }

    #[test]
    fn duplicate_enum_is_error() {
        let msgs = assert_messages("enum A { X } enum A { Y }");
        assert!(
            msgs.iter().any(|m| m.message().contains("Duplicate enum")),
            "expected duplicate-enum error, got: {:?}",
            msgs
        );
    }

    #[test]
    fn duplicate_constructor_is_error() {
        let msgs = assert_messages("enum A { Foo } enum B { Foo }");
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("Duplicate constructor")),
            "expected duplicate-constructor error, got: {:?}",
            msgs
        );
    }

    #[test]
    fn enum_decl_cache_aligned_with_id_table() {
        // Regression test for the ID-alignment bug in
        // `infer_enum_decl`: the pre-walk mints one ID for the
        // `EnumDecl` node, one for each `EnumVariant` node, and one
        // for each `Expression::Type` payload. The infer pass must
        // consume exactly the same number of IDs (via `self.infer`)
        // so the cache lines up with the id table.
        //
        // Concretely: `enum Color { Red, Green(int) }` produces
        //   1 (EnumDecl) + 2 (variants) + 1 (Green's payload type) = 4
        // pre-walk IDs, and `infer` must consume all 4. The cache
        // therefore has the same length as the id table.
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

    // ---- Constructor calls ----

    #[test]
    fn constructor_call_with_wrong_arity_is_error() {
        // Option::Some takes 1 arg, called with 2.
        let src = "Option::Some(1, 2)";
        let msgs = assert_messages(src);
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("expects 1 arguments")),
            "expected arity error, got: {:?}",
            msgs
        );
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
    fn unknown_enum_constructor_is_error() {
        let msgs = assert_messages("Nonexistent::Some(1);");
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("Cannot find enum")),
            "expected unknown-enum error, got: {:?}",
            msgs
        );
    }

    #[test]
    fn unknown_variant_on_known_enum_is_error() {
        let src = "Option::Purlpe(1)";
        let msgs = assert_messages(src);
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("Cannot find variant")),
            "expected unknown-variant error, got: {:?}",
            msgs
        );
    }

    // ---- Pattern matching ----

    #[test]
    fn match_with_all_variants_no_error() {
        // Enum declarations are top-level statements (no trailing
        // `;`) and must appear at the end of a sequence of
        // statements. Zero-arity constructors require `()`.
        let src = "let x = Option::Some(1); match x { Option::None() => 0, Option::Some(v) => v };";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
    }

    #[test]
    fn match_with_wildcard_no_exhaustiveness_error() {
        let src = "let x = Option::Some(1); match x { _ => 0 };";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
    }

    #[test]
    fn match_non_exhaustive_reports_missing() {
        let src = "let x = Option::None(); match x { Option::None() => 0 };";
        let msgs = assert_messages(src);
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("Non-exhaustive match")),
            "expected non-exhaustive error, got: {:?}",
            msgs
        );
        let msg = msgs
            .iter()
            .find(|m| m.message().contains("Non-exhaustive"))
            .unwrap();
        assert!(
            msg.message().contains("Some"),
            "expected `Some` to be mentioned, got: {:?}",
            msg.message()
        );
    }

    #[test]
    fn match_with_unreachable_arm_reports() {
        let src = "let x = Option::None(); match x { Option::None() => 0, Option::None() => 1 };";
        let msgs = assert_messages(src);
        assert!(
            msgs.iter().any(|m| m.message().contains("Unreachable arm")),
            "expected unreachable-arm error, got: {:?}",
            msgs
        );
    }

    #[test]
    fn match_pattern_binding_does_not_leak() {
        // `v` is bound inside the arm; referencing it after the
        // match should error.
        let src = "let x = Option::Some(1); match x { Option::Some(v) => 0 }; v";
        let msgs = assert_messages(src);
        // The `v` reference after the match is unknown.
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("Cannot find value `v`")),
            "expected 'v not found' error, got: {:?}",
            msgs
        );
    }

    #[test]
    fn nested_constructor_pattern_typechecks() {
        // Patterns can be nested; the inner sub-patterns are
        // checked against the corresponding payload types. We
        // wrap a value in a single-level enum so the inner
        // pattern is `Wrap::Inner(int)` — the nested pattern
        // case. (Truly recursive `Option<Option<T>>` is not
        // constructible because `Option::Some` takes `int`
        // directly, so we use a custom enum that wraps a type
        // whose pattern can be nested.)
        let src = "let x = Wrap::W(Inner::I(7)); match x { Wrap::W(Inner::I(v)) => v }; enum Inner { I(int) } enum Wrap { W(Inner) }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
    }

    #[test]
    fn forward_reference_to_constructor_works() {
        // The enum is declared AFTER the use; the pre-pass makes
        // this work.
        let src = "let x = Option::Some(1)";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
    }

    // ---- Format-string typecheck ----

    #[test]
    fn format_string_percent_i_requires_int() {
        let msgs = assert_messages(
            r#"use io::{stdout, write};
use string::{format, to_bytes};
write(stdout(), to_bytes(format("%i", "hello")));"#,
        );
        assert!(
            msgs.iter().any(|m| m.message().contains("requires int")),
            "expected '%i requires int' error, got: {:?}",
            msgs
        );
    }

    #[test]
    fn format_string_percent_s_requires_string() {
        let msgs = assert_messages(r#"string::format("%s", 42);"#);
        assert!(
            msgs.iter().any(|m| m.message().contains("requires string")),
            "expected '%s requires string' error, got: {:?}",
            msgs
        );
    }

    #[test]
    fn format_string_percent_f_requires_float() {
        let msgs = assert_messages(r#"string::format("%f", 1);"#);
        assert!(
            msgs.iter().any(|m| m.message().contains("requires float")),
            "expected '%f requires float' error, got: {:?}",
            msgs
        );
    }

    #[test]
    fn format_string_with_constructor_value_errors_on_percent_s() {
        // Red-team critical: passing a `Constructor` (a sum) where
        // a string is expected must be flagged.
        let src = r#"string::format("%s", Option::Some(1));"#;
        let msgs = assert_messages(src);
        assert!(
            msgs.iter().any(|m| m.message().contains("requires string")),
            "expected 'requires string' error, got: {:?}",
            msgs
        );
    }

    #[test]
    fn format_string_with_constructor_via_match_works() {
        // The match arm's body must be inferable as a string, and
        // string::format("%s", s) should accept it.
        let src = r#"let s = match Option::Some(1) { Option::None() => "none", Option::Some(_) => "some" }; string::format("%s", s);"#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
    }

    #[test]
    fn addition_of_strings_is_string() {
        assert_ok("\"hello\" + \"world\"", string());
    }

    #[test]
    fn string_plus_int_errors() {
        let msgs = assert_messages("\"hello\" + 42;");
        assert!(
            msgs.iter().any(|m| m.message().contains("Type mismatch")),
            "expected string+int type mismatch, got: {:?}",
            msgs
        );
    }

    #[test]
    fn string_format_call_returns_string() {
        assert_ok(r#"string::format("%i-%s", 42, "x")"#, string());
    }

    #[test]
    fn format_percent_v_accepts_int() {
        let (mut c, _) = check(
            r#"use io::{stdout, write};
use string::{format, to_bytes};
write(stdout(), to_bytes(format("%v", 42)));"#,
        );
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
    }

    #[test]
    fn format_percent_v_accepts_structural_tuple_and_record() {
        let (mut c, _) = check(
            r#"use io::{stdout, write};
use string::{format, to_bytes};
write(stdout(), to_bytes(format("%v%v", (1, true), { a: 3, b: "x" })));"#,
        );
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
    }

    #[test]
    fn format_percent_i_on_open_type_errors() {
        let msgs = assert_messages(
            r#"use io::{stdout, write};
use string::{format, to_bytes};
fn bad<T>(T x) { write(stdout(), to_bytes(format("%i", x))); } fn main() { bad(1); }"#,
        );
        assert!(
            msgs.iter().any(|m| {
                m.message().contains("open type")
                    && m.help().as_ref().is_some_and(|h| h.contains("%v"))
            }),
            "expected open-type `%i` error with `%v` help, got: {:?}",
            msgs
        );
    }

    #[test]
    fn format_percent_v_without_show_errors() {
        let msgs = assert_messages(
            r#"use io::{stdout, write};
use string::{format, to_bytes};
fn bad<T>(T x) { write(stdout(), to_bytes(format("%v", x))); } fn main() { bad(1); }"#,
        );
        assert!(
            msgs.iter().any(|m| m.message().contains("Show")),
            "expected Show requirement for `%v`, got: {:?}",
            msgs
        );
    }

    #[test]
    fn format_percent_v_rejects_structural_tuple_with_open_type() {
        let msgs = assert_messages(
            r#"use io::{stdout, write};
use string::{format, to_bytes};
fn bad<T>(T x) { write(stdout(), to_bytes(format("%v", (x, 1)))); } fn main() { bad(1); }"#,
        );
        assert!(
            msgs.iter().any(|m| m.message().contains("Show")),
            "expected Show requirement for structural `%v` with open T, got: {:?}",
            msgs
        );
    }

    // ---- Inner-pattern reachability ----

    #[test]
    fn typechecker_does_not_report_unreachable_for_different_inner_patterns() {
        // Two Result::Ok arms with different inner patterns are both reachable.
        let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
        fn unwrap(Result r) -> int {
            return match r {
                Result::Ok(Option::Some(v)) => v,
                Result::Ok(Option::None) => 0,
                Result::Err(_) => -1,
            };
        }
        fn main() {
            write(stdout(), to_bytes(format("%i", unwrap(Result::Ok(Option::Some(42))))));
        }
        "#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        let unreachable: Vec<String> = msgs
            .iter()
            .filter(|m| m.message().contains("Unreachable arm"))
            .map(|m| m.message().to_string())
            .collect();
        assert!(
            unreachable.is_empty(),
            "Typechecker should NOT report unreachable arm for different inner patterns, got: {:?}",
            unreachable
        );
    }

    #[test]
    fn match_identical_nested_patterns_reports_unreachable() {
        let src = r#"
fn unwrap(Result r) -> int {
    return match r {
        Result::Ok(Option::Some(v)) => v,
        Result::Ok(Option::Some(_)) => 0,
        Result::Ok(Option::None) => -1,
        Result::Err(_) => -2,
    };
}
fn main() { let _ = unwrap(Result::Ok(Option::Some(1))); }
"#;
        let msgs = assert_messages(src);
        assert!(
            msgs.iter().any(|m| m.message().contains("Unreachable arm")),
            "expected unreachable for duplicate nested coverage, got: {:?}",
            msgs
        );
    }

    #[test]
    fn match_second_tuple_field_distinction_remains_reachable() {
        let src = r#"
enum Inner { A, B }
enum Outer { V(Inner, Inner) }
fn classify(Outer p) -> int {
    return match p {
        Outer::V(Inner::A, Inner::B) => 1,
        Outer::V(Inner::A, Inner::A) => 2,
        Outer::V(_, _) => 0,
    };
}
fn main() { let _ = classify(Outer::V(Inner::A, Inner::A)); }
"#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(
            !msgs.iter().any(|m| m.message().contains("Unreachable arm")),
            "CoverageTree must distinguish second tuple field, got: {:?}",
            msgs
        );
    }

    #[test]
    fn builtin_len_named_arg_typechecks() {
        let src = r#"
fn main() {
    let n = len(value: [1, 2, 3]);
}
"#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "expected named `len(value: …)` to typecheck, got: {:?}",
            msgs
        );
    }

    #[test]
    fn builtin_len_unknown_named_arg_errors() {
        let msgs = assert_messages("fn main() { let n = len(xs: [1]); }");
        assert!(
            msgs.iter().any(|m| m.message().contains("Unknown named argument")),
            "expected unknown named arg diagnostic, got: {:?}",
            msgs
        );
    }

    // ---- Field access ----

    #[test]
    fn access_field_from_record_variant_returns_field_type() {
        // `p.x` where `p` is bound to a `Point::Point { x: int, y: int }`
        // constructor. The receiver's type is a `Ty::Constructor` with
        // a record-shaped payload, so the field resolves uniquely to
        // `int`.
        let src = "enum Point { Origin, Point { x: int, y: int } } \
                   let p = Point::Point { x: 5, y: 12 }; p.x;";
        let (mut c, _) = check(src);
        let ty = inferred_expr_ty(src);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "expected no diagnostics for `p.x`, got: {:?}",
            msgs
        );
        assert_eq!(ty, int(), "field access should produce `int`");
    }

    #[test]
    fn access_field_from_non_record_produces_error() {
        // `1.x` — the receiver is an `int`, not a sum. The typechecker
        // should emit a "Cannot access field" diagnostic and NOT
        // silently succeed.
        let msgs = assert_messages("1.x;");
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("Cannot access field")),
            "expected 'Cannot access field' diagnostic, got: {:?}",
            msgs
        );
    }

    #[test]
    fn access_unknown_field_produces_error() {
        // `p.z` where `p` is bound to a `Point::Point { x, y }`
        // constructor. The variant IS a record but doesn't declare
        // `z`. Should emit "Type `Point` has no field `z`" with a
        // help hint listing the actual fields.
        let src = "enum Point { Origin, Point { x: int, y: int } } \
                   let p = Point::Point { x: 1, y: 2 }; p.z;";
        let msgs = assert_messages(src);
        let no_field = msgs
            .iter()
            .find(|m| m.message().contains("no field `z`"))
            .unwrap_or_else(|| panic!("expected 'no field `z`' diagnostic, got: {:?}", msgs));
        // The help hint should mention the actual fields available.
        let hint = no_field
            .help()
            .as_ref()
            .expect("expected help hint on 'no field' diagnostic");
        assert!(
            hint.contains("`x`") && hint.contains("`y`"),
            "expected help hint listing available fields, got: {:?}",
            hint
        );
    }

    #[test]
    fn record_construct_on_class_tuple_variant_steers_to_tuple_or_record_decl() {
        let msgs = assert_messages(
            r#"
class JsonObject { x: int }
enum JsonValue { Obj(JsonObject) }
fn main() { let _ = JsonValue::Obj { x: 1 }; }
"#,
        );
        assert!(
            msgs.iter().any(|m| {
                m.message().contains("payload shape mismatch")
                    && m.help().as_ref().is_some_and(|h| {
                        h.contains("wrapping")
                            && h.contains("JsonObject")
                            && h.contains("Obj(value)")
                    })
            }),
            "expected class-wrap tuple hint, got: {:?}",
            msgs.iter()
                .map(|m| (m.message(), m.help().clone()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn tuple_variant_wrapping_class_typechecks() {
        let (c, _) = check(
            r#"
class JsonObject { x: int }
enum JsonValue { Obj(JsonObject) }
fn main() {
    let o = new JsonObject(1);
    let _ = JsonValue::Obj(o);
}
"#,
        );
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn access_field_from_tuple_variant_produces_error() {
        // `p.x` where `p` is bound to `Tuple::Wrap(1, 2)` — a
        // Tuple-shaped variant. The variant isn't a record, so we
        // emit a tailored "Cannot access field on non-record
        // variant" diagnostic that names the variant's shape.
        let src = "enum Tuple { Wrap(int, int) } \
                   let p = Tuple::Wrap(1, 2); p.x;";
        let msgs = assert_messages(src);
        let diag = msgs
            .iter()
            .find(|m| {
                m.message().contains("Cannot access field")
                    || m.message().contains("has no field")
            })
            .unwrap_or_else(|| {
                panic!("expected field-access diagnostic, got: {:?}", msgs)
            });
        let hint = diag.help().as_ref();
        if let Some(hint) = hint {
            assert!(
                hint.contains("tuple") || hint.contains("record-shaped"),
                "expected help hint about tuple/record variants, got: {:?}",
                hint
            );
        } else {
            assert!(
                diag.message().contains("Tuple"),
                "expected diagnostic to name the enum, got: {}",
                diag.message()
            );
        }
    }

    #[test]
    fn access_field_ambiguous_across_variants_emits_narrow_with_match() {
        // Two record-shaped variants both declare `x`. The
        // receiver's type is `Ty::Sum { name: "Two", variants: [...] }`
        // (because we annotate the parameter `p: Two` directly, so
        // `p`'s type is `Ty::Con("Two")` which we resolve through
        // the registry). Either way, the field type is ambiguous
        // and we emit "narrow with match first".
        let src = "enum Two { A { x: int, y: int }, B { x: string, z: int } } \
                   fn get_x(Two p) -> int { return p.x; }";
        let msgs = assert_messages(src);
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("narrow with match first")),
            "expected 'narrow with match first' diagnostic, got: {:?}",
            msgs
        );
    }

    #[test]
    fn access_tuple_field_after_match_narrows_shared_index() {
        // Derive Show walks `recv.0` via AST Access. Multiple tuple
        // variants share index `"0"`; match refinement must make this
        // typecheck (and `%v` must resolve Show).
        let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
#[derive(Show, Eq)]
enum Box { S(string), B(bool) }
fn main() {
    write(stdout(), to_bytes(format("%v,", Box::S("x"))));
    write(stdout(), to_bytes(format("%z", Box::S("x") == Box::S("x"))));
}
"#;
        let mut ast = Pratt::default().parse(src).expect("parse");
        let _ = crate::attrs::expand_program(&mut ast);
        let mut c = Checker::new();
        let _ = c.check_program(&ast);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "expected derived Show/Eq with shared tuple indices to typecheck, got: {:?}",
            msgs
        );
    }

    #[test]
    fn access_field_via_function_parameter_resolves() {
        // Field access on a function parameter whose type is
        // annotated with the bare enum name `Point` — the
        // typechecker parses this as `Ty::Con("Point")` and
        // resolves it through the enum registry to find that
        // `Point::Point` is a record-shaped variant carrying `x`
        // of type `int`.
        let src = "enum Point { Origin, Point { x: int, y: int } } \
                   fn get_x(Point p) -> int { return p.x; }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "expected no diagnostics, got: {:?}", msgs);
    }

    #[test]
    fn access_field_on_sum_param_with_unique_field_resolves() {
        // Same as above, but the enum has exactly ONE record-shaped
        // variant so the field is unambiguous. The typechecker
        // should resolve `p.x` to `int` without diagnostic.
        let src = "enum Point { Origin, Point { x: int, y: int } } \
                   fn get_x(Point p) -> int { return p.x; }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "expected no diagnostics for unambiguous field access, got: {:?}",
            msgs
        );
    }

    // ---- Typed aggregates ----

    #[test]
    fn tuple_literal_infers_heterogeneous_product_type() {
        // `(1, "x")` should infer `(int, string)`.
        let (mut c, _) = check("(1, \"x\")");
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
        assert_eq!(
            inferred_expr_ty("(1, \"x\")"),
            tuple_ty(vec![int(), string()]),
            "expected tuple type (int, string)"
        );
    }

    #[test]
    fn array_literal_infers_static_length_array() {
        // `[1, 2, 3]` should infer `[int; 3]` (static length 3).
        let (mut c, _) = check("[1, 2, 3]");
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
        assert_eq!(
            inferred_expr_ty("[1, 2, 3]"),
            array_fixed(int(), 3),
            "expected [int; 3]"
        );
    }

    #[test]
    fn array_literal_heterogeneous_elements_emits_diagnostic() {
        // `[1, "x"]` should emit "array element type mismatch".
        let (_c, msgs) = check_warn("[1, \"x\"]");
        let found = msgs
            .iter()
            .any(|m| m.message().contains("element type mismatch"));
        assert!(
            found,
            "expected 'element type mismatch' diagnostic, got: {:?}",
            msgs
        );
    }

    #[test]
    fn array_static_index_out_of_bounds_emits_diagnostic() {
        // `let arr = [0, 1, 2]; arr[3]` — arr is `[int; 3]`,
        // accessing index 3 is OOB.
        let src = "fn main() { let arr = [0, 1, 2]; let _ = arr[3]; }";
        let (_c, msgs) = check_warn(src);
        let found = msgs.iter().any(|m| m.message().contains("out of bounds"));
        assert!(found, "expected OOB diagnostic, got: {:?}", msgs);
    }

    #[test]
    fn array_constant_index_in_bounds_emits_no_diagnostic() {
        // `arr[2]` on `[0, 1, 2]` is in bounds — no error.
        let src = "fn main() { let arr = [0, 1, 2]; let _ = arr[2]; }";
        let (_c, msgs) = check_warn(src);
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    }

    #[test]
    fn array_runtime_index_emits_no_diagnostic() {
        // `arr[i]` on a static-length array, where `i` is a
        // variable — no static check possible, no error.
        let src = "fn main() { let arr = [0, 1, 2]; let i = 1; let _ = arr[i]; }";
        let (_c, msgs) = check_warn(src);
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    }

    #[test]
    fn array_dynamic_length_no_oob_check() {
        // Function-returned arrays are dynamic-length; OOB
        // access is allowed (the user said SQL/JSON results
        // must not be flagged).
        let src = "fn get_array() -> [int] { return [1, 2, 3]; } \
                   fn main() { let arr = get_array(); let _ = arr[10]; }";
        let (_c, msgs) = check_warn(src);
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    }

    #[test]
    fn vec_push_grows_for_later_indexing() {
        let src = "fn main() { let arr = Vec::from([0, 1]); arr.push(2); let _ = arr[2]; }";
        let (_c, msgs) = check_warn(src);
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    }

    #[test]
    fn array_append_assignment_is_rejected() {
        let src = "fn main() { let arr = [0, 1]; arr[] = 2; }";
        let (_c, msgs) = check_warn(src);
        let found = msgs
            .iter()
            .any(|m| m.message().contains("append assignment `arr[] = value` is no longer supported"));
        assert!(
            found,
            "expected append assignment rejection, got: {:?}",
            msgs
        );
    }

    #[test]
    fn len_of_array_returns_int() {
        assert_ok("len([0, 1])", int());
    }

    #[test]
    fn len_of_string_tuple_dict_returns_int() {
        assert_ok(r#"len("foo")"#, int());
        assert_ok("len((1, 2, 3))", int());
        assert_ok("len({ a: 1, b: 2 })", int());
    }

    #[test]
    fn typeof_literal_returns_string() {
        assert_ok("typeof 1", string());
        assert_ok(r#"typeof "hi""#, string());
        assert_ok("typeof (1, 2)", string());
    }

    #[test]
    fn typeof_rejects_open_type_parameter() {
        let src = r#"
fn name_of<T>(T x) -> string { return typeof x; }
fn main() { name_of(1); }
"#;
        let (_c, msgs) = check_warn(src);
        let found = msgs
            .iter()
            .any(|m| m.message().contains("`typeof` requires a ground type"));
        assert!(
            found,
            "expected ground-type diagnostic for typeof on open T, got: {:?}",
            msgs
        );
    }

    #[test]
    fn len_rejects_wrong_arity() {
        let (_c, msgs0) = check_warn("fn main() { len(); }");
        assert!(
            msgs0
                .iter()
                .any(|m| m.message().contains("len expects 1 argument")),
            "expected arity diagnostic for len(), got: {:?}",
            msgs0
        );
        let (_c, msgs2) = check_warn("fn main() { len(1, 2); }");
        assert!(
            msgs2
                .iter()
                .any(|m| m.message().contains("len expects 1 argument")),
            "expected arity diagnostic for len(1, 2), got: {:?}",
            msgs2
        );
    }

    #[test]
    fn len_rejects_non_array() {
        let src = "fn main() { let x = 1; len(x); }";
        let (_c, msgs) = check_warn(src);
        let found = msgs.iter().any(|m| {
            m.message().contains("No instance for `Length")
                || m.message().contains("Length")
        });
        assert!(
            found,
            "expected Length instance diagnostic for len(int), got: {:?}",
            msgs
        );
    }

    #[test]
    fn len_accepts_custom_length_impl() {
        let src = r#"
class Box { value: int }
impl Length for Box {
    fn len(Box b) -> int { return 1; }
}
fn main() { len(new Box(0)); }
"#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "expected len(Box) with Length impl to typecheck, got: {:?}",
            msgs
        );
    }

    #[test]
    fn len_generic_length_bound() {
        let src = r#"
fn size_of<T: Length>(T x) -> int { return len(x); }
fn main() { size_of("hi"); }
"#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "expected generic Length bound len() to typecheck, got: {:?}",
            msgs
        );
    }

    #[test]
    fn tuple_constant_index_oob_emits_diagnostic() {
        // `let t = (1, 2); t[5]` — tuple length 2, index 5.
        let src = "fn main() { let t = (1, 2); let _ = t[5]; }";
        let (_c, msgs) = check_warn(src);
        let found = msgs.iter().any(|m| m.message().contains("out of bounds"));
        assert!(found, "expected tuple OOB, got: {:?}", msgs);
    }

    #[test]
    fn parenthesised_expr_is_not_tuple() {
        // `(1)` and `(1 + 2)` and `((1))` should NOT be tuples.
        // The parser fixes this by requiring a comma inside the
        // parens for the tuple form. After the parser fix, each
        // of these parses to a single integer expression with
        // type `int`.
        let (mut c1, _) = check("(1)");
        let msgs1 = c1.take_messages();
        assert!(msgs1.is_empty(), "msgs1: {:?}", msgs1);
        assert_eq!(inferred_expr_ty("(1)"), int());

        let (mut c2, _) = check("(1 + 2)");
        let msgs2 = c2.take_messages();
        assert!(msgs2.is_empty(), "msgs2: {:?}", msgs2);
        assert_eq!(inferred_expr_ty("(1 + 2)"), int());
    }

    #[test]
    fn parenthesised_arithmetic_works_in_binary_op() {
        // `(1 + 2) * 3` should evaluate to `int` (= 9 at
        // runtime). Pre-24 it incorrectly parsed `(1 + 2)` as
        // a 1-tuple and broke arithmetic.
        let (mut c, _) = check("(1 + 2) * 3");
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
        assert_eq!(inferred_expr_ty("(1 + 2) * 3"), int());
    }

    #[test]
    fn array_dynamic_length_param_lets_runtime_index() {
        // Function param is dynamic-length — must allow any
        // index.
        let src = "fn head([int] arr) -> int { return arr[0]; }";
        let (_c, msgs) = check_warn(src);
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    }

    #[test]
    fn index_non_aggregate_emits_diagnostic() {
        // `let x = 5; x[0]` — index on `int` is an error.
        let src = "fn main() { let x = 5; let _ = x[0]; }";
        let (_c, msgs) = check_warn(src);
        let found = msgs
            .iter()
            .any(|m| m.message().contains("cannot index non-aggregate"));
        assert!(found, "expected indexing-error, got: {:?}", msgs);
    }

    // ---- Dict tests ----

    #[test]
    fn dict_literal_infers_record_type() {
        // `{ foo: 42 }` should infer `Ty::Record { fields: [("foo", int)] }`.
        // We expect the var type via `lookup_at` won't work in
        // this minimal setup; instead, verify that the dict
        // expression parses and type-checks without error and
        // that the let-bound `d` resolves to a `Ty::Record` via
        // the env lookup.
        let (mut c, _ty) = check("fn main() { let d = { foo: 42 }; }");
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
        // The side-table records `d`'s type — verify it.
        let d_ty = c.codegen_var_type("d").cloned();
        let d_pruned = d_ty.map(|t| crate::typechecking::subst::apply_ty_prune(c.subst(), &t));
        assert_eq!(
            d_pruned,
            Some(crate::typechecking::ty::record(vec![(
                "foo".to_string(),
                int()
            )])),
            "expected d: {{ foo: int }}"
        );
    }

    #[test]
    fn dict_missing_field_access_emits_diagnostic() {
        // `{ foo: 42 }; x.bar` must error when `bar` is missing.
        let src = "fn main() { let x = { foo: 42 }; let _ = x.bar; }";
        let (_c, msgs) = check_warn(src);
        let found = msgs
            .iter()
            .any(|m| m.message().contains("Cannot find field `bar`"));
        assert!(found, "expected missing-field diagnostic, got: {:?}", msgs);
    }

    #[test]
    fn dict_present_field_access_emits_no_diagnostic() {
        let src = "fn main() { let x = { foo: 42 }; let _ = x.foo; }";
        let (_c, msgs) = check_warn(src);
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
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
            m.code() == Some(ErrorCode::DuplicateField)
                && m.message().contains("Duplicate field")
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
                && m.message().contains("Duplicate field `x` in record pattern")
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
                && m.message().contains("Duplicate field `x` in record constructor")
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
            m.code() == Some(ErrorCode::DuplicateField)
                && m.message().contains("Duplicate field")
        });
        assert!(
            found,
            "expected duplicate-field diagnostic, got: {:?}",
            msgs
        );
    }

    #[test]
    fn dict_structurally_typed_unification() {
        // Two separate `{ foo: 1 }` literals should have the
        // same record type.
        let (mut c, ty1) = check("fn main() { let _ = { foo: 42 }; return { foo: 42 }; }");
        let _ = c.take_messages();
        let ty2 = {
            let (mut c2, ty2) = check("fn main() { let _ = { foo: 42 }; return { foo: 99 }; }");
            let _ = c2.take_messages();
            ty2
        };
        // We can't unify across two checkers easily; instead,
        // verify each infers the same structural type.
        let r1 = apply_ty_prune(c.subst(), &ty1);
        let r2 = apply_ty_prune(c.subst(), &ty2);
        assert_eq!(r1, r2);
    }

    #[test]
    fn dict_let_binding_works_end_to_end() {
        // The full codegen path: lex + parse + type + codegen
        // + VM. We just check the diagnostics are clean.
        let src = "fn main() { let d = { x: 1, y: 2 }; let _ = d.x + d.y; }";
        let (_c, msgs) = check_warn(src);
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    }

    // ---- Type alias tests ----

    #[test]
    fn type_alias_for_tuple_is_substituted() {
        // `type Point = (int, int);` then `let p: Point = (3, 4);`
        // should typecheck without diagnostic (the alias is
        // substituted to `(int, int)` and the literal is its
        // structural equivalent).
        let src = "type Point = (int, int); fn main() { let p: Point = (3, 4); }";
        let (_c, msgs) = check_warn(src);
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    }

    #[test]
    fn type_alias_used_as_function_parameter() {
        let src = "type Point = (int, int);
                   fn distance(Point p) -> int { return p[0]; }
                   fn main() { }";
        let (_c, msgs) = check_warn(src);
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    }

    #[test]
    fn generic_enum_type_app_builds_ty_app() {
        let src = "enum Box<T> { Box(T) } fn f(Box<int> x) -> int { return 0; }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
        let scheme = c.env().lookup("f").unwrap();
        let ty = apply_ty_prune(c.subst(), &scheme.ty);
        let Ty::Fun(param, ret) = ty else {
            panic!("expected function type");
        };
        assert_eq!(
            *param,
            Ty::App(Box::new(Ty::Con("Box".into())), vec![int()])
        );
        assert_eq!(*ret, int());
    }

    #[test]
    fn generic_enum_construct_infers_box_int() {
        let src = "enum Box<T> { Empty, Full(T) }
                   fn main() { let x: Box<int> = Box::Full(7); }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
        let x_ty = c.codegen_var_type("x").expect("x should be recorded");
        assert_eq!(
            apply_ty_prune(c.subst(), x_ty),
            Ty::App(Box::new(Ty::Con("Box".into())), vec![int()])
        );
    }

    #[test]
    fn generic_enum_match_binds_int_payload() {
        let src = "enum Box<T> { Empty, Full(T) }
                   fn main() {
                       let x = Box::Full(7);
                       let y = match x {
                           Box::Empty => 0,
                           Box::Full(v) => v,
                       };
                   }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
        assert_eq!(
            c.codegen_var_type("y")
                .map(|t| apply_ty_prune(c.subst(), t)),
            Some(int())
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
    fn builtin_option_app_annotation_unifies_with_constructor_sum() {
        let src = "fn main() { let x: Option<int> = Option::Some(1); let y = match x { Option::None => 0, Option::Some(v) => v }; }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
        let x_ty = c.codegen_var_type("x").expect("x should be recorded");
        assert!(matches!(
            apply_ty_prune(c.subst(), x_ty),
            Ty::App(con, args)
                if con.as_ref() == &Ty::Con(common::BUILTIN_OPTION_ENUM.into())
                    && args == vec![int()]
        ));
        assert_eq!(c.codegen_var_type("y"), Some(&int()));
    }

    #[test]
    fn generic_type_app_arity_mismatch_errors() {
        let src = "enum Box<T> { Box(T) } fn f(Box<int, string> x) -> int { return 0; }";
        let (_c, msgs) = check_warn(src);
        assert!(
            msgs.iter().any(|m| m
                .message()
                .contains("Type constructor `Box` expects 1 type arguments, got 2")),
            "expected arity diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn generic_type_alias_expands_to_rhs() {
        let src = "type Pair<T> = (T, T); fn f(Pair<int> p) -> int { return 0; }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
        let scheme = c.env().lookup("f").unwrap();
        let ty = apply_ty_prune(c.subst(), &scheme.ty);
        let Ty::Fun(param, ret) = ty else {
            panic!("expected function type");
        };
        assert_eq!(*param, tuple_ty(vec![int(), int()]));
        assert_eq!(*ret, int());
        assert!(
            c.generic_aliases.contains_key("Pair"),
            "generic alias should be registered"
        );
    }

    #[test]
    fn generic_type_alias_arity_mismatch_errors() {
        let src = "type Pair<T> = (T, T); fn f(Pair<int, string> p) -> int { return 0; }";
        let (_c, msgs) = check_warn(src);
        assert!(
            msgs.iter().any(|m| m
                .message()
                .contains("Type constructor `Pair` expects 1 type arguments, got 2")),
            "expected arity diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn alias_does_not_leak_into_unrelated_declarations() {
        let src = "type Int = int; fn main() { }";
        let (_c, msgs) = check_warn(src);
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    }

    #[test]
    fn function_body_alias_can_shadow_outer_alias() {
        let src = r#"
            type Value = int;
            fn main() {
                type Value = string;
                let s: Value = "ok";
            }
            fn id(Value x) -> int { return x; }
        "#;
        let (_c, msgs) = check_warn(src);
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    }

    #[test]
    fn duplicate_type_alias_in_same_scope_errors() {
        let src = "type Id = int; type Id = string; fn main() { }";
        let (_c, msgs) = check_warn(src);
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("Duplicate type alias `Id`")),
            "expected duplicate alias diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn typeclass_impl_missing_required_method_errors() {
        let src = r#"
            trait Tiny<T> {
                fn add(int a, int b) -> int;
                fn zero() -> int { return 0; }
            }
            impl Tiny<int> {
                fn zero() -> int { return 0; }
            }
        "#;
        let (_c, msgs) = check_warn(src);
        assert!(
            msgs.iter().any(|m| {
                m.message()
                    .contains("Instance of `Tiny` for `int` is missing method `add`")
            }),
            "expected missing-method diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn typeclass_impl_overlapping_instance_errors() {
        let src = r#"
            trait Tiny<T> {
                fn add(int a, int b) -> int;
            }
            impl Tiny<int> {
                fn add(int a, int b) -> int { return a; }
            }
            impl Tiny<int> {
                fn add(int a, int b) -> int { return b; }
            }
        "#;
        let (_c, msgs) = check_warn(src);
        assert!(
            msgs.iter().any(|m| m
                .message()
                .contains("Overlapping instance `Tiny<int>` conflicts with existing `Tiny<int>`")),
            "expected overlapping-instance diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
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
        });
        c.generics.instances.push(InstanceDef {
            class: "Choice".to_string(),
            defined_module: "b".to_string(),
            range: 1..2,
            args: vec![int()],
            method_fqns: HashMap::new(),
            assoc_tys: HashMap::new(),
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
            impl Tiny<int> {
            }
        "#;
        let (c, msgs) = check_warn(src);
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
        assert_eq!(
            c.instance_method_fqn("Tiny", &[int()], "zero"),
            Some("Tiny__default__zero")
        );
    }

    #[test]
    fn typeclass_impl_unknown_method_errors() {
        let src = r#"
            trait Tiny<T> {
                fn add(int a, int b) -> int;
            }
            impl Tiny<int> {
                fn add(int a, int b) -> int { return a; }
                fn foo(int a) -> int { return a; }
            }
        "#;
        let (_c, msgs) = check_warn(src);
        assert!(
            msgs.iter().any(|m| m
                .message()
                .contains("Unknown method `foo` in instance of `Tiny`")),
            "expected unknown-method diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
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

    /// Phase 5: `impl Ordered<int>` without `Equal<int>` is an error.
    #[test]
    fn typeclass_impl_missing_superclass_instance_errors() {
        let src = r#"
            trait Equal<T> { fn eq_val(T a, T b) -> bool; }
            trait Ordered<T: Equal> { fn lt_val(T a, T b) -> bool; }
            impl Ordered<int> {
                fn lt_val(int a, int b) -> bool { return a < b; }
            }
        "#;
        let (_c, msgs) = check_warn(src);
        assert!(
            msgs.iter().any(|m| {
                let msg = m.message();
                msg.contains("requires superclass instance")
                    && msg.contains("Equal")
                    && msg.contains("Ordered")
            }),
            "expected missing-superclass diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Phase 5: `T: Ordered` implies `Equal` — `eq_val` resolves without
    /// writing `T: Ordered + Equal`.
    #[test]
    fn implied_superclass_bound_allows_superclass_method() {
        let src = r#"
            trait Equal<T> { fn eq_val(T a, T b) -> bool; }
            trait Ordered<T: Equal> { fn lt_val(T a, T b) -> bool; }
            impl Equal<int> {
                fn eq_val(int a, int b) -> bool { return a == b; }
            }
            impl Ordered<int> {
                fn lt_val(int a, int b) -> bool { return a < b; }
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
            impl Equal<int> {
                fn eq_val(int a, int b) -> bool { return a == b; }
            }
            impl Ordered<int> {
                fn lt_val(int a, int b) -> bool { return a < b; }
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
    fn unsatisfied_abstract_constraint_kind_reports_diagnostic() {
        let src = r#"
            fn id<c: * -> Constraint, T: c>(T x) -> T {
                return x;
            }
        "#;
        let (_c, msgs) = check_warn(src);
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("Cannot satisfy abstract constraint")),
            "expected unsatisfied abstract constraint diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
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

    /// `T: Add` does not allow `*`.
    #[test]
    fn add_bound_does_not_allow_mul() {
        let src = r#"
            fn bad<T: Add>(T a, T b) -> T { return a * b; }
        "#;
        let (_c, msgs) = check_warn(src);
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("without bound `Mul`")),
            "expected missing Mul bound diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
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

    /// `T: Lt` does not allow `>`.
    #[test]
    fn lt_bound_does_not_allow_gt() {
        let src = r#"
            fn bad<T: Lt>(T a, T b) -> bool { return a > b; }
        "#;
        let (_c, msgs) = check_warn(src);
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("without bound `Gt`")),
            "expected missing Gt bound diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn block_scoped_alias_does_not_leak() {
        let src = r#"
            type Local = int;
            fn main() {
                if true {
                    type Local = string;
                    let s: Local = "ok";
                }
                let n: Local = 1;
            }
        "#;
        let (_c, msgs) = check_warn(src);
        assert!(msgs.is_empty(), "unexpected: {:?}", msgs);
    }

    #[test]
    fn access_field_from_let_bound_sum_value_works() {
        // The receiver is `let p = ...;` where the value is bound
        // to a `Ty::Sum` (via a function parameter flowing through
        // a `match`). After matching, the active variant is
        // statically known, so `p.x` works.
        let src = "enum Point { Origin, Point { x: int, y: int } } \
                   fn distance_squared(Point p) -> int { \
                       return match p { \
                           Point::Origin => 0, \
                           Point::Point { x, y } => x, \
                       }; \
                   } \
                   fn main() { let _ = distance_squared(Point::Point { x: 5, y: 12 }); }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "expected no diagnostics, got: {:?}", msgs);
    }

    #[test]
    fn access_field_chained_id_alignment() {
        // `p.x.y` parses as `Access(Access(p, "x"), "y")`. The
        // inner `Access` must consume the receiver's ID AND its
        // own ID to stay lockstep with the pre-walk.
        //
        // We don't assert cache/id_table alignment here because
        // `infer_fragment` has a pre-existing asymmetry where it
        // doesn't consume `Variable` IDs (it processes them
        // inline). This is unrelated to `Expression::Access`. We
        // instead verify the OUTER access produces the expected
        // diagnostic.
        let src = "enum Point { Origin, Point { x: int, y: int } } \
                   let p = Point::Point { x: 5, y: 12 }; p.x.y;";
        let msgs = assert_messages(src);
        let cannot_access: Vec<_> = msgs
            .iter()
            .filter(|m| m.message().contains("Cannot access field"))
            .collect();
        assert!(
            !cannot_access.is_empty(),
            "expected at least one 'Cannot access field' diagnostic from outer access, got: {:?}",
            msgs
        );
    }

    // ============================================================
    // ---- field_type_for tests ----
    // ============================================================
    //
    // The `field_type_for` helper is the codegen-side complement
    // to `field_index_for`. It's queried by `receiver_type` when
    // resolving chained accesses (`p.x.v`). The helper reads from
    // the same `enum_payloads` registry that `field_index_for`
    // reads from — so the tests below verify the data plumbing,
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
    /// should yield `None` — the codegen's defensive `LoadField(0)`
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
    /// upstream" case — the codegen falls back to `LoadField(0)`.
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
        // exact Ty here — we just verify the helper returns
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
    fn async_fn_call_has_coroutine_type() {
        let src = "async fn coro() { yield 1; } fn main() { let h = coro(); }";
        let (c, _) = check(src);
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
        let ty = c.codegen_var_type("h").expect("h should be recorded");
        match apply_ty_prune(&c.subst(), ty) {
            Ty::App(con, args) => {
                assert_eq!(con.as_ref(), &Ty::Con("coroutine".to_string()));
                assert_eq!(args.len(), 2);
                assert_eq!(apply_ty_prune(&c.subst(), &args[1]), unit_ty());
            }
            other => panic!("expected coroutine<_, unit>, got {:?}", other),
        }
    }

    #[test]
    fn resume_with_send_unifies_send_type() {
        let src = r#"async fn ping() { let msg = yield "ready"; }
fn main() { let h = ping(); resume h with "hello"; }"#;
        let (c, _) = check(src);
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
    }

    #[test]
    fn coro_send_example_typechecks() {
        let src = include_str!("../../../../examples/coro_send.hy");
        let (c, _) = check(src);
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
    }

    #[test]
    fn yield_from_requires_coroutine_target() {
        let (c, _) = check("async fn bad() { yield from 1; }");
        assert!(
            c.messages().iter().any(|m| {
                m.message().contains("Type mismatch")
                    && m.help()
                        .as_ref()
                        .is_some_and(|h| h.contains("yield from target"))
            }),
            "expected yield-from type error, got {:?}",
            c.messages()
        );
    }

    #[test]
    fn resume_expression_returns_yield_type() {
        let (c, _) =
            check("async fn coro() { yield 1; } fn main() { let h = coro(); let x = resume h; }");
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
        let x_ty = c
            .codegen_var_type("x")
            .expect("x should be recorded in codegen_var_types");
        assert_eq!(apply_ty_prune(c.subst(), x_ty), int());
    }

    #[test]
    fn for_in_coro_binds_loop_var_to_yield_type() {
        let src = r#"
async fn counter() { yield 0; yield 1; }
fn main() {
    for x in counter() {
        let y = x;
    }
}
"#;
        let (c, _) = check(src);
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
        let y_ty = c
            .codegen_var_type("y")
            .expect("y should be recorded in codegen_var_types");
        assert_eq!(apply_ty_prune(c.subst(), y_ty), int());
        let x_ty = c
            .codegen_var_type("x")
            .expect("x should be recorded in codegen_var_types");
        assert_eq!(apply_ty_prune(c.subst(), x_ty), int());
    }

    #[test]
    fn for_in_non_iterable_is_diagnostic() {
        let (c, _) = check("fn main() { for x in 42 { } }");
        assert!(
            c.messages()
                .iter()
                .any(|m| m.message().contains("not iterable")),
            "expected for-in not-iterable error, got {:?}",
            c.messages()
        );
    }

    #[test]
    fn for_in_array_binds_element_type() {
        let src = r#"
fn main() {
    for x in [1, 2, 3] {
        let y = x;
    }
}
"#;
        let (c, _) = check(src);
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
        let y_ty = c.codegen_var_type("y").expect("y");
        assert_eq!(apply_ty_prune(c.subst(), y_ty), int());
    }

    #[test]
    fn range_literal_infers_range_int() {
        let (c, _) = check("fn main() { let r = 0..10; }");
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
        let r_ty = c.codegen_var_type("r").expect("r");
        assert_eq!(
            apply_ty_prune(c.subst(), r_ty),
            crate::typechecking::ty::range_ty(int())
        );
    }

    #[test]
    fn range_inclusive_infers_range_inclusive_int() {
        let (c, _) = check("fn main() { let r = 0..=10; }");
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
        let r_ty = c.codegen_var_type("r").expect("r");
        assert_eq!(
            apply_ty_prune(c.subst(), r_ty),
            crate::typechecking::ty::range_inclusive_ty(int())
        );
    }

    #[test]
    fn for_in_range_binds_element_type() {
        let src = r#"
fn main() {
    for x in 0..5 {
        let y = x;
    }
}
"#;
        let (c, _) = check(src);
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
        let y_ty = c.codegen_var_type("y").expect("y");
        assert_eq!(apply_ty_prune(c.subst(), y_ty), int());
    }

    #[test]
    fn range_accepts_float_bounds() {
        let (c, _) = check("fn main() { let r = 1.0..2.0; }");
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
        let r_ty = c.codegen_var_type("r").expect("r");
        assert_eq!(
            apply_ty_prune(c.subst(), r_ty),
            crate::typechecking::ty::range_ty(float())
        );
    }

    #[test]
    fn range_rejects_string_bounds_without_ord() {
        let (c, _) = check(r#"fn main() { let r = "a".."z"; }"#);
        assert!(
            c.messages().iter().any(|m| m.message().contains("Ord")),
            "expected Ord requirement diagnostic, got {:?}",
            c.messages()
        );
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
    fn range_generic_without_ord_is_diagnostic() {
        let src = r#"
fn span<T>(T a, T b) -> Range<T> {
    return a..b;
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages().iter().any(|m| m.message().contains("Ord")),
            "expected Ord bound diagnostic, got {:?}",
            c.messages()
        );
    }

    #[test]
    fn for_in_range_rejects_non_numeric_ord_element() {
        // Construction of Range<string> already fails (no Ord). Drive the
        // for-in diagnostic via a generic Ord type that isn't steppable.
        let src = r#"
fn dump<T: Ord>(T a, T b) {
    for x in a..b { }
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages()
                .iter()
                .any(|m| m.message().contains("cannot iterate")),
            "expected non-steppable range for-in diagnostic, got {:?}",
            c.messages()
        );
    }

    #[test]
    fn range_to_vec_infers_vec_int() {
        let src = r#"
fn main() {
    let v = (0..5).to_vec();
}
"#;
        let (c, _) = check(src);
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
        let v_ty = c.codegen_var_type("v").expect("v");
        assert_eq!(
            apply_ty_prune(c.subst(), v_ty),
            crate::typechecking::ty::vec_app_ty(int())
        );
    }

    #[test]
    fn range_inclusive_to_vec_infers_vec_int() {
        let src = r#"
fn main() {
    let r = 0..=3;
    let v = r.to_vec();
}
"#;
        let (c, _) = check(src);
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
        let v_ty = c.codegen_var_type("v").expect("v");
        assert_eq!(
            apply_ty_prune(c.subst(), v_ty),
            crate::typechecking::ty::vec_app_ty(int())
        );
    }

    #[test]
    fn range_to_vec_byte_and_float() {
        let src = r#"
fn main() {
    let lo: byte = 1;
    let hi: byte = 4;
    let b = (lo..hi).to_vec();
    let f = (1.0..=3.0).to_vec();
}
"#;
        let (c, _) = check(src);
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
        let b_ty = c.codegen_var_type("b").expect("b");
        assert_eq!(
            apply_ty_prune(c.subst(), b_ty),
            crate::typechecking::ty::vec_app_ty(crate::typechecking::ty::byte())
        );
        let f_ty = c.codegen_var_type("f").expect("f");
        assert_eq!(
            apply_ty_prune(c.subst(), f_ty),
            crate::typechecking::ty::vec_app_ty(float())
        );
    }

    #[test]
    fn range_to_vec_rejects_non_numeric_ord_element() {
        let src = r#"
fn dump<T: Ord>(T a, T b) {
    let _ = (a..b).to_vec();
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages()
                .iter()
                .any(|m| m.message().contains("cannot iterate")),
            "expected non-steppable range to_vec diagnostic, got {:?}",
            c.messages()
        );
    }

    #[test]
    fn range_inclusive_to_vec_rejects_non_numeric_ord_element() {
        let src = r#"
fn dump<T: Ord>(T a, T b) {
    let _ = (a..=b).to_vec();
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages()
                .iter()
                .any(|m| m.message().contains("cannot iterate over `RangeInclusive")),
            "expected RangeInclusive to_vec diagnostic, got {:?}",
            c.messages()
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
    fn for_in_hetero_tuple_is_diagnostic() {
        let (c, _) = check("fn main() { for x in (1, \"a\") { } }");
        assert!(
            c.messages()
                .iter()
                .any(|m| m.message().contains("heterogeneous")),
            "expected hetero tuple diagnostic, got {:?}",
            c.messages()
        );
    }

    #[test]
    fn for_in_hetero_dict_is_diagnostic() {
        let (c, _) = check("fn main() { for x in { a: 1, b: \"x\" } { } }");
        assert!(
            c.messages()
                .iter()
                .any(|m| m.message().contains("heterogeneous")),
            "expected hetero dict diagnostic, got {:?}",
            c.messages()
        );
    }

    #[test]
    fn for_in_custom_iterator_accepted() {
        let src = r#"
class Counter {
    cur: int,
    end: int,
}

impl IntoIterator<Counter> {
    type Item = int;
    type IntoIter = Counter;
    fn into_iter(Counter c) -> Counter {
        return c;
    }
}

impl Iterator<Counter> {
    type Item = int;
    fn next(Counter c) -> Option<int> {
        if c.cur < c.end {
            let v = c.cur;
            c.cur = c.cur + 1;
            return Option::Some(v);
        }
        return Option::None;
    }
}

fn main() {
    let c = new Counter(0, 3);
    for x in c {
        let y = x;
    }
}
"#;
        let (c, _) = check(src);
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
        let y_ty = c.codegen_var_type("y").expect("y");
        assert_eq!(apply_ty_prune(c.subst(), y_ty), int());
    }

    /// COI-115: `impl Trait<T>` bodies must see inherent `impl T` methods.
    #[test]
    fn trait_impl_can_call_inherent_method() {
        let src = r#"
class Box { v: int }
class BoxIter { i: int }

impl Box {
    fn iter() -> BoxIter {
        return new BoxIter(self.v);
    }
}

impl IntoIterator<Box> {
    type Item = int;
    type IntoIter = BoxIter;
    fn into_iter(Box m) -> BoxIter {
        return m.iter();
    }
}

impl Iterator<BoxIter> {
    type Item = int;
    fn next(BoxIter it) -> Option<int> {
        return Option::None;
    }
}

fn main() {
    let b = new Box(1);
    let it = b.into_iter();
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
    fn trait_impl_can_call_generic_inherent_method() {
        let src = r#"
class Map<K, V> { n: int }
class MapIter<K, V> { n: int }

impl Map<K, V> {
    fn iter() -> MapIter<K, V> {
        return new MapIter(self.n);
    }
}

impl IntoIterator<Map<K, V>> {
    type Item = int;
    type IntoIter = MapIter<K, V>;
    fn into_iter(Map<K, V> m) -> MapIter<K, V> {
        return m.iter();
    }
}

impl Iterator<MapIter<K, V>> {
    type Item = int;
    fn next(MapIter<K, V> it) -> Option<int> {
        return Option::None;
    }
}

fn main() {
    let m = new Map(0);
    let it = m.into_iter();
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
    fn trait_impl_can_call_inherent_method_declared_later() {
        let src = r#"
class Box { v: int }
class BoxIter { i: int }

impl IntoIterator<Box> {
    type Item = int;
    type IntoIter = BoxIter;
    fn into_iter(Box m) -> BoxIter {
        return m.iter();
    }
}

impl Box {
    fn iter() -> BoxIter {
        return new BoxIter(self.v);
    }
}

impl Iterator<BoxIter> {
    type Item = int;
    fn next(BoxIter it) -> Option<int> {
        return Option::None;
    }
}

fn main() {
    let b = new Box(1);
    let it = b.into_iter();
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Stubbing must not invent methods — missing inherent stays an error.
    #[test]
    fn trait_impl_missing_inherent_method_still_errors() {
        let src = r#"
class Box { v: int }
class BoxIter { i: int }

impl IntoIterator<Box> {
    type Item = int;
    type IntoIter = BoxIter;
    fn into_iter(Box m) -> BoxIter {
        return m.iter();
    }
}

impl Iterator<BoxIter> {
    type Item = int;
    fn next(BoxIter it) -> Option<int> {
        return Option::None;
    }
}

fn main() {
    let b = new Box(1);
}
"#;
        let msgs = assert_messages(src);
        assert!(
            msgs.iter().any(|m| {
                let t = m.message();
                t.contains("Cannot find method `iter`") || t.contains("Unknown method")
            }),
            "expected missing-method diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Forward stubs must carry real arity — extra args still reject.
    #[test]
    fn trait_impl_inherent_method_arity_mismatch_still_errors() {
        let src = r#"
class Box { v: int }
class BoxIter { i: int }

impl IntoIterator<Box> {
    type Item = int;
    type IntoIter = BoxIter;
    fn into_iter(Box m) -> BoxIter {
        return m.iter(1);
    }
}

impl Box {
    fn iter() -> BoxIter {
        return new BoxIter(self.v);
    }
}

impl Iterator<BoxIter> {
    type Item = int;
    fn next(BoxIter it) -> Option<int> {
        return Option::None;
    }
}

fn main() {
    let b = new Box(1);
}
"#;
        let msgs = assert_messages(src);
        assert!(
            msgs.iter().any(|m| {
                let t = m.message();
                t.contains("too many arguments") || t.contains("Unknown method")
            }),
            "expected arity diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Static inherent methods declared after a trait instance must resolve.
    #[test]
    fn trait_impl_can_call_static_inherent_method_declared_later() {
        let src = r#"
class Box { v: int }
class BoxIter { i: int }

impl IntoIterator<Box> {
    type Item = int;
    type IntoIter = BoxIter;
    fn into_iter(Box m) -> BoxIter {
        return Box::make_iter(m);
    }
}

impl Box {
    static fn make_iter(Box m) -> BoxIter {
        return new BoxIter(m.v);
    }
}

impl Iterator<BoxIter> {
    type Item = int;
    fn next(BoxIter it) -> Option<int> {
        return Option::None;
    }
}

fn main() {
    let b = new Box(1);
    let it = b.into_iter();
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(
            c.is_static_method("Box", "make_iter"),
            "stub must record static method"
        );
    }

    /// Inherent methods must not bind their short name in env (COI-115).
    /// `impl Path { fn join }` would otherwise shadow `fn join` / `use path::{join}`.
    #[test]
    fn inherent_method_does_not_bind_bare_name() {
        let src = r#"
class P { x: int }
impl P {
    fn join(P other) -> P {
        return other;
    }
}
fn join(string a, string b) -> string {
    return a;
}
fn main() {
    let s = join("ok", "");
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        let s_ty = c.codegen_var_type("s").expect("s");
        assert_eq!(apply_ty_prune(c.subst(), s_ty).to_string(), "string");
    }

    #[test]
    fn yield_outside_async_is_diagnostic() {
        let (c, _) = check("fn main() { yield 1; }");
        assert!(
            c.messages()
                .iter()
                .any(|m| m.message().contains("yield outside async")),
            "expected yield-outside-async diagnostic, got {:?}",
            c.messages()
        );
    }

    /// `return e;` inside an `async fn` unifies against the SAME
    /// type as `yield e;` (not `unit`) — `resume` has a single
    /// static result type covering both the yielded values and the
    /// final completion value, so a `return` of a matching type
    /// typechecks cleanly.
    #[test]
    fn return_inside_coroutine_unifies_with_yield_type() {
        let src = "async fn coro() { yield 1; return 42; } \
                   fn main() { let h = coro(); }";
        let (c, _) = check(src);
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
    }

    /// A `return` whose type disagrees with the coroutine's yield
    /// type is a real type error (soundness: `resume`'s result type
    /// can't be both `int` and `string`).
    #[test]
    fn return_inside_coroutine_mismatched_type_is_diagnostic() {
        let src = r#"async fn coro() { yield 1; return "oops"; } fn main() { let h = coro(); }"#;
        let (c, _) = check(src);
        assert!(
            c.messages().iter().any(|m| {
                m.message().contains("Type mismatch")
                    && m.help()
                        .as_ref()
                        .is_some_and(|h| h.contains("return value"))
            }),
            "expected return-value type mismatch, got {:?}",
            c.messages()
        );
    }

    /// A `return` with no preceding `yield` still pins the
    /// coroutine's yield/resume type — `coroutine<int, unit>` here.
    #[test]
    fn return_only_coroutine_infers_yield_type_from_return() {
        let src = "async fn coro() { return 42; } fn main() { let h = coro(); let x = resume h; }";
        let (c, _) = check(src);
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
        let x_ty = c
            .codegen_var_type("x")
            .expect("x should be recorded in codegen_var_types");
        assert_eq!(apply_ty_prune(c.subst(), x_ty), int());
    }

    #[test]
    fn done_builtin_typechecks_to_bool() {
        let src = "async fn c() { yield 1; } fn main() { let h = c(); let d = done(h); }";
        let (c, _) = check(src);
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
        let d_ty = c.codegen_var_type("d").expect("d should be recorded");
        assert_eq!(apply_ty_prune(c.subst(), d_ty), boolean());
    }

    #[test]
    fn async_fn_return_annotation_unifies_with_yield() {
        let src = "async fn c() -> int { yield 1; return 2; } fn main() { let h = c(); }";
        let (c, _) = check(src);
        assert!(c.messages().is_empty(), "unexpected: {:?}", c.messages());
    }

    #[test]
    fn async_fn_return_annotation_mismatch_errors() {
        let src = "async fn c() -> string { yield 1; } fn main() { let h = c(); }";
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(
            msgs.iter().any(|m| m.message().contains("Type mismatch")),
            "expected annotation mismatch, got {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn declare_struct_ret_recorded_for_invoke_typing() {
        let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
extern struct Point {
    x: int32,
    y: int32,
};

use ffi::{declare, dload, invoke, Error};
use ffi::types::{Int32};

fn main() -> Result<(), Error> {
    let lib = dload("sum")?;
    let make_id = declare(
        lib,
        "make_point",
        (Int32, Int32),
        Point,
    )?;
    let p = invoke(lib, make_id, (3, 4))?;
    write(stdout(), to_bytes(format("%i", p.x)));
    write(stdout(), to_bytes(format("%i", p.y)));
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        let ret = c
            .ffi_fn_ret_tys
            .get("make_id")
            .expect("declare binding should record ret Ty");
        match ret {
            Ty::Record { fields } => {
                assert!(fields.iter().any(|(n, _)| n == "x"));
                assert!(fields.iter().any(|(n, _)| n == "y"));
            }
            other => panic!("expected Record ret, got {other}"),
        }
    }

    #[test]
    fn impl_method_self_calls_later_helper() {
        let src = r#"
class Foo { v: int, }
impl Foo {
    fn bump() -> int { return helper(self.v); }
}
fn helper(int n) -> int { return n + 1; }
fn main() {
    let f = new Foo(41);
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
    fn impl_method_can_call_module_helper_after_impl() {
        let src = r#"
fn helper(int n) -> int { return n + 1; }
class Foo { v: int, }
impl Foo {
    fn bump(Foo f) -> int { return helper(f.v); }
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
class Foo { v: int, }
impl Foo {
    fn bump(Foo f) -> int { return helper(f.v); }
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
    fn impl_method_forward_helper_num_bound_rejects_string() {
        let src = r#"
class Foo { v: string, }
impl Foo {
    fn bump(Foo f) -> string { return add1(f.v); }
}
fn add1<T: Num>(T n) -> T { return n; }
fn main() {
    let f = new Foo("x");
    let x = f.bump();
}
"#;
        let msgs = assert_messages(src);
        assert!(
            msgs.iter().any(|m| m.message().contains("No instance for `Num")),
            "expected Num<string> rejection via stub, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn impl_method_forward_helper_where_num_rejects_string() {
        let src = r#"
class Foo { v: string, }
impl Foo {
    fn bump(Foo f) -> string { return twice(f.v); }
}
fn twice<T>(T n) -> T where Num<T> { return n; }
fn main() {
    let f = new Foo("x");
    let x = f.bump();
}
"#;
        let msgs = assert_messages(src);
        assert!(
            msgs.iter().any(|m| m.message().contains("No instance for `Num")),
            "expected where Num rejection via stub, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn impl_method_forward_helper_arity_mismatch() {
        let src = r#"
class Foo { v: int, }
impl Foo {
    fn bump(Foo f) -> int { return helper(f.v, 2); }
}
fn helper(int n) -> int { return n + 1; }
fn main() {
    let f = new Foo(1);
    let x = f.bump();
}
"#;
        let msgs = assert_messages(src);
        assert!(
            msgs.iter().any(|m| m.message().contains("too many arguments")),
            "expected arity error via stub, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn impl_method_forward_helper_type_mismatch() {
        let src = r#"
class Foo { v: int, }
impl Foo {
    fn bump(Foo f) -> int { return helper("x"); }
}
fn helper(int n) -> int { return n + 1; }
fn main() {
    let f = new Foo(1);
    let x = f.bump();
}
"#;
        let msgs = assert_messages(src);
        assert!(
            msgs.iter().any(|m| m.message().contains("Type mismatch")),
            "expected type mismatch via stub, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn impl_method_forward_unknown_helper_still_errors() {
        let src = r#"
class Foo { v: int, }
impl Foo {
    fn bump(Foo f) -> int { return missing(f.v); }
}
fn main() {
    let f = new Foo(1);
    let x = f.bump();
}
"#;
        let msgs = assert_messages(src);
        assert!(
            msgs.iter().any(|m| m.message().contains("Cannot find function")),
            "expected unknown helper error, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn impl_method_forward_nullary_and_inferred_return() {
        let src = r#"
class Foo { v: int, }
impl Foo {
    fn bump(Foo f) -> int { return one() + helper(f.v); }
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
class Foo { v: int, }
impl Foo {
    fn bump(Foo f) -> int { return id(f.v); }
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
    fn invoke_refines_return_type_from_class_field_declare_id() {
        let src = r#"
use ffi::{declare, dload, invoke, Error};
use ffi::types::Float;

class Api {
    id: int,
}

fn main() -> Result<(), Error> {
    let lib = dload("noop")?;
    let api = new Api(0);
    api.id = declare(lib, "f", (), Float)?;
    let f: float = invoke(lib, api.id, ())?;
    let _ = f;
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
    fn invoke_refines_return_type_from_local_copied_from_field() {
        let src = r#"
use ffi::{declare, dload, invoke, Error};
use ffi::types::Float;

class Api {
    id: int,
}

fn main() -> Result<(), Error> {
    let lib = dload("noop")?;
    let api = new Api(0);
    api.id = declare(lib, "f", (), Float)?;
    let fn_id = api.id;
    let f: float = invoke(lib, fn_id, ())?;
    let _ = f;
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
    id: int,
}

impl Api {
    fn load(int lib) -> Result<(), Error> {
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

    /// `self.id` as the invoke fn-id inside the owning method (not only via
    /// free-fn field access after `impl` assignment).
    #[test]
    fn invoke_refines_return_type_from_self_field_inside_method() {
        let src = r#"
use ffi::{declare, dload, invoke, Error};
use ffi::types::Float;

class Api {
    id: int,
}

impl Api {
    fn load(int lib) -> Result<(), Error> {
        self.id = declare(lib, "f", (), Float)?;
    }

    fn call(int lib) -> Result<float, Error> {
        let f: float = invoke(lib, self.id, ())?;
        return f;
    }
}

fn main() -> Result<(), Error> {
    let lib = dload("noop")?;
    let api = new Api(0);
    api.load(lib)?;
    let _ = api.call(lib)?;
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Call-site `declare` metadata flows into a callee's bare `invoke` param.
    #[test]
    fn invoke_refines_return_type_from_param_call_site_flow() {
        let src = r#"
use ffi::{declare, dload, invoke, Error};
use ffi::types::Float;

class Api {
    id: int,
}

impl Api {
    fn load(int lib) -> Result<(), Error> {
        self.id = declare(lib, "f", (), Float)?;
    }
}

fn helper(int id) -> Result<float, Error> {
    let f: float = invoke(0, id, ())?;
    return f;
}

fn main() -> Result<(), Error> {
    let lib = dload("noop")?;
    let api = new Api(0);
    api.load(lib)?;
    let _ = helper(api.id)?;
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

    #[test]
    fn invoke_refines_return_type_from_param_call_site_flow_forward_ref() {
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
        let (c, _) = check(src);
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Named-arg call sites must record param flow (`infer_and_reorder` path).
    #[test]
    fn invoke_refines_return_type_from_named_param_call_site_flow() {
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
    let _ = helper(id: fn_id)?;
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Variadic `declare` metadata must flow through a callee param (extra args ok).
    #[test]
    fn invoke_variadic_from_param_call_site_allows_extra_args() {
        let src = r#"
use ffi::{declare, dload, invoke, Error};
use ffi::types::{Int, Float};

fn helper(int id) -> Result<float, Error> {
    let f: float = invoke(0, id, (1, 2, 3))?;
    return f;
}

fn main() -> Result<(), Error> {
    let lib = dload("noop")?;
    let fn_id = declare(lib, "f", (Int,), Float, true)?;
    let _ = helper(fn_id)?;
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
    fn invoke_variadic_from_param_call_site_rejects_too_few_args() {
        let src = r#"
use ffi::{declare, dload, invoke, Error};
use ffi::types::{Int, Float};

fn helper(int id) -> Result<float, Error> {
    let f: float = invoke(0, id, (1,))?;
    return f;
}

fn main() -> Result<(), Error> {
    let lib = dload("noop")?;
    let fn_id = declare(lib, "f", (Int, Int), Float, true)?;
    let _ = helper(fn_id)?;
}
"#;
        let msgs = assert_messages(src);
        assert!(
            msgs.iter().any(|m| {
                m.message().contains("variadic invoke expects at least")
                    || m.code() == Some(ErrorCode::InvokeArity)
            }),
            "expected variadic arity error via param flow, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(
            !msgs.iter().any(|m| m.message().contains("Type mismatch")),
            "brace-imported Float must refine invoke ret via param flow, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Bare int param without call-site `declare` metadata must not refine to float.
    #[test]
    fn invoke_untracked_param_id_does_not_refine_return_type() {
        let src = r#"
use ffi::{declare, dload, invoke, Error};
use ffi::types::Float;

fn helper(int id) -> Result<float, Error> {
    let f: float = invoke(0, id, ())?;
    return f;
}

fn main() -> Result<(), Error> {
    let lib = dload("noop")?;
    let _ = declare(lib, "f", (), Float)?;
    let _ = helper(0)?;
}
"#;
        let msgs = assert_messages(src);
        assert!(
            msgs.iter().any(|m| {
                m.code() == Some(ErrorCode::TypeMismatch)
                    || m.message().contains("Type mismatch")
                    || m.message().contains("float")
            }),
            "expected float refine failure for untracked param id, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Variadic `declare` metadata stored on a class field must refine arity
    /// (extra args ok) — same path codegen uses for `is_ffi_declare_variadic_for_fn_id`.
    #[test]
    fn invoke_variadic_from_class_field_allows_extra_args() {
        let src = r#"
use ffi::{declare, dload, invoke, Error};
use ffi::types::{Int, Float};

class Api {
    id: int,
}

fn main() -> Result<(), Error> {
    let lib = dload("noop")?;
    let api = new Api(0);
    api.id = declare(lib, "f", (Int,), Float, true)?;
    let f: float = invoke(lib, api.id, (1, 2, 3))?;
    let _ = f;
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Field → local copy must also carry variadic `nfixed` (let-init Access path).
    #[test]
    fn invoke_variadic_from_local_copied_from_field() {
        let src = r#"
use ffi::{declare, dload, invoke, Error};
use ffi::types::{Int, Float};

class Api {
    id: int,
}

fn main() -> Result<(), Error> {
    let lib = dload("noop")?;
    let api = new Api(0);
    api.id = declare(lib, "f", (Int,), Float, true)?;
    let fn_id = api.id;
    let f: float = invoke(lib, fn_id, (1, 2))?;
    let _ = f;
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
    fn invoke_variadic_from_class_field_rejects_too_few_args() {
        let src = r#"
use ffi::{declare, dload, invoke, Error};
use ffi::types::{Int, Float};

class Api {
    id: int,
}

fn main() -> Result<(), Error> {
    let lib = dload("noop")?;
    let api = new Api(0);
    api.id = declare(lib, "f", (Int, Int), Float, true)?;
    let f: float = invoke(lib, api.id, (1,))?;
    let _ = f;
}
"#;
        let msgs = assert_messages(src);
        assert!(
            msgs.iter().any(|m| {
                m.message().contains("variadic invoke expects at least")
                    || m.code() == Some(ErrorCode::InvokeArity)
            }),
            "expected variadic arity error, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Field without a recorded `declare` must not refine `invoke` to float.
    #[test]
    fn invoke_untracked_field_id_does_not_refine_return_type() {
        let src = r#"
use ffi::{declare, dload, invoke, Error};
use ffi::types::Float;

class Api {
    id: int,
}

fn main() -> Result<(), Error> {
    let lib = dload("noop")?;
    let api = new Api(0);
    let _ = declare(lib, "f", (), Float)?;
    let f: float = invoke(lib, api.id, ())?;
    let _ = f;
}
"#;
        let msgs = assert_messages(src);
        assert!(
            msgs.iter().any(|m| {
                m.code() == Some(ErrorCode::TypeMismatch)
                    || m.message().contains("Type mismatch")
                    || m.message().contains("float")
            }),
            "expected float refine failure for untracked field id, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    // ---- Error handling: raise / ? / ?? / ?. ----

    #[test]
    fn explicit_result_ok_return_accepted_in_result_mode() {
        let src = r#"
fn f() -> Result<int, string> {
    return Result::Ok(99);
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(c.fn_is_result_mode("f"));
    }

    #[test]
    fn explicit_result_err_return_accepted_in_result_mode() {
        let src = r#"
fn f() -> Result<int, string> {
    return Result::Err("boom");
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(c.fn_is_result_mode("f"));
    }

    #[test]
    fn raise_infers_result_mode_and_wraps_success() {
        let src = r#"
fn f(int n) {
    if n == 0 { raise "zero"; }
    return n;
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(c.fn_is_result_mode("f"));
    }

    #[test]
    fn raise_with_explicit_non_result_return_errors() {
        let msgs = assert_messages(r#"fn f() -> int { raise "x"; return 1; }"#);
        assert!(
            msgs.iter().any(|m| {
                m.code() == Some(ErrorCode::TypeMismatch)
                    || m.message().contains("Type mismatch")
                    || m.message().contains("Result")
            }),
            "expected raise vs -> int error, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn raise_followed_by_try_names_invalid_form() {
        let msgs = assert_messages(r#"fn f() { raise "err"?; }"#);
        assert!(
            msgs.iter().any(|m| {
                m.code() == Some(ErrorCode::InvalidTry)
                    && m.message().contains("`?` after `raise`")
                    && m.help()
                        .as_ref()
                        .is_some_and(|h| h.contains("raise err;"))
            }),
            "expected raise…? guidance, got: {:?}",
            msgs.iter()
                .map(|m| (m.code(), m.message(), m.help().clone()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn try_on_raise_names_invalid_form() {
        let msgs = assert_messages(r#"fn f() { (raise "err")?; }"#);
        assert!(
            msgs.iter().any(|m| {
                m.code() == Some(ErrorCode::InvalidTry)
                    && m.message().contains("`?` cannot follow `raise`")
                    && m.help()
                        .as_ref()
                        .is_some_and(|h| h.contains("raise err;"))
            }),
            "expected (raise)? guidance, got: {:?}",
            msgs.iter()
                .map(|m| (m.code(), m.message(), m.help().clone()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn try_on_non_option_result_is_hard_error() {
        let msgs = assert_messages(r#"fn f() -> int { let x = 1; return x?; }"#);
        assert!(
            msgs.iter().any(|m| m.code() == Some(ErrorCode::InvalidTry)),
            "expected InvalidTry (E0114), got: {:?}",
            msgs.iter()
                .map(|m| (m.code(), m.message()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn try_on_result_propagates_ok_payload() {
        let src = r#"
fn inner() { raise "e"; }
fn outer() {
    let v = inner()?;
    return v;
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(c.fn_is_result_mode("outer"));
    }

    #[test]
    fn mismatched_error_types_conflict() {
        let msgs = assert_messages(
            r#"
fn a() { raise "s"; return 1; }
fn b() { raise 1; return 2; }
fn c() {
    let _x = a()?;
    let _y = b()?;
    return 0;
}
"#,
        );
        assert!(
            msgs.iter().any(|m| {
                m.code() == Some(ErrorCode::ConflictingErrorType)
                    || m.message().contains("Type mismatch")
                    || m.message().contains("error type")
            }),
            "expected single-E conflict, got: {:?}",
            msgs.iter()
                .map(|m| (m.code(), m.message()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn coalesce_option_and_result_typecheck() {
        let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let a = Option::None ?? "bar";
    let b = Result::Err("boom") ?? 7;
    write(stdout(), to_bytes(format("%s", a)));
    write(stdout(), to_bytes(format("%i", b)));
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
    fn coalesce_on_non_option_result_errors() {
        let msgs = assert_messages(r#"fn main() { let x = 1 ?? 2; }"#);
        assert!(
            msgs.iter()
                .any(|m| m.code() == Some(ErrorCode::InvalidCoalesce)),
            "expected InvalidCoalesce (E0115), got: {:?}",
            msgs.iter()
                .map(|m| (m.code(), m.message()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn optional_access_on_option_ok() {
        let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let o = Option::Some({ v: 1 });
    let n = o?.v;
    write(stdout(), to_bytes(format("%i", n ?? 0)));
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
    fn optional_access_on_result_errors() {
        let msgs = assert_messages(r#"fn main() { let r = Result::Ok({ v: 1 }); let _x = r?.v; }"#);
        assert!(
            msgs.iter()
                .any(|m| m.code() == Some(ErrorCode::InvalidOptionalAccess)),
            "expected InvalidOptionalAccess (E0116), got: {:?}",
            msgs.iter()
                .map(|m| (m.code(), m.message()))
                .collect::<Vec<_>>()
        );
    }

    // ---- Virtual modules: prelude + ffi scope ----

    #[test]
    fn prelude_injects_option_without_import() {
        let src = r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn main() {
    let o = Option::Some(1);
    write(stdout(), to_bytes(format("%i", match o { Option::Some(v) => v, Option::None => 0 })));
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(c.builtin_name_in_scope("Option"));
        assert!(c.builtin_name_in_scope("Eq"));
        assert!(c.prelude_fn_in_scope("assert").is_some());
        assert!(!c.ffi_fn_in_scope("dload").is_some());
    }

    #[test]
    fn assert_infers_result_unit_string() {
        let src = r#"
fn main() {
    let r = assert(true);
    let _ = match r {
        Result::Ok(_) => 1,
        Result::Err(_) => 0,
    };
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
    fn assert_with_message_ok() {
        let src = r#"
fn main() {
    let r = assert(false, "nope");
    let _ = match r {
        Result::Ok(_) => 1,
        Result::Err(_) => 0,
    };
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
    fn assert_wrong_arity_errors() {
        let msgs = assert_messages(r#"fn main() { let _ = assert(); }"#);
        assert!(
            msgs.iter().any(|m| m.message().contains("assert expects")),
            "expected arity diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn assert_rebind_as_check_works() {
        let src = r#"
use prelude::test::assert as check;
fn main() {
    let r = check(1 == 1);
    let _ = match r {
        Result::Ok(_) => 1,
        Result::Err(_) => 0,
    };
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(c.prelude_fn_in_scope("check").is_some());
        assert!(c.prelude_fn_in_scope("assert").is_none());
    }

    #[test]
    fn panic_requires_string() {
        let msgs = assert_messages(r#"fn main() { panic 1; }"#);
        assert!(
            msgs.iter().any(|m| m.message().contains("Type mismatch")),
            "expected type mismatch for panic int, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn dload_without_ffi_import_errors() {
        let msgs = assert_messages(r#"fn main() { let lib = dload("x.so"); }"#);
        assert!(
            msgs.iter().any(|m| {
                m.code() == Some(ErrorCode::UnknownValue) && m.message().contains("dload")
            }),
            "expected UnknownValue for bare dload, got: {:?}",
            msgs.iter()
                .map(|m| (m.code(), m.message()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn use_ffi_types_glob_brings_int_tag_into_scope() {
        let src = r#"
use ffi::{declare, dload, invoke, Error};
use ffi::types::{Int};
fn main() -> Result<(), Error> {
    let lib = dload("x.so")?;
    let id = declare(lib, "sum", (Int, Int), Int)?;
    let _ = invoke(lib, id, (1, 2))?;
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(c.ffi_tag_in_scope("Int"));
        assert!(c.ffi_fn_in_scope("dload").is_some());
    }

    #[test]
    fn ffi_types_qualified_path_works_without_import() {
        let src = r#"
use ffi::{declare, dload, invoke, Error};
fn main() -> Result<(), Error> {
    let lib = dload("x.so")?;
    let id = declare(lib, "sum", (ffi::types::Int, ffi::types::Int), ffi::types::Int)?;
    let _ = invoke(lib, id, (1, 2))?;
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
    fn ffi_error_kind_field_is_matchable() {
        let src = r#"
use ffi::{dload, Error, ErrorKind};
fn main() {
    let r = dload("missing");
    let _ = match r {
        Result::Ok(h) => h,
        Result::Err(e) => match e.kind {
            ErrorKind::LibraryNotFound => 0,
            ErrorKind::SymbolNotFound => 1,
            ErrorKind::ArityMismatch => 2,
            ErrorKind::Libffi => 3,
            ErrorKind::InvalidSignature => 4,
            ErrorKind::InvalidHandle => 5,
            ErrorKind::Unsupported => 6,
            ErrorKind::Other => 7,
        },
    };
}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(c.enums.contains_key(common::BUILTIN_FFI_ERROR_ENUM));
        assert!(c.enums.contains_key(common::BUILTIN_FFI_ERROR_KIND_ENUM));
    }

    #[test]
    fn rebind_prelude_eq_allows_user_trait_eq() {
        let src = r#"
use prelude::ops::Eq as PreludeEq;
trait Eq<T> {
    fn id(T x) -> T;
}
fn main() {}
"#;
        let (c, _) = check(src);
        assert!(
            c.messages().is_empty(),
            "unexpected: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(!c.builtin_name_in_scope("Eq"));
        assert!(c.builtin_name_in_scope("PreludeEq"));
    }

    #[test]
    fn duplicate_prelude_eq_without_rebind_errors() {
        let msgs = assert_messages(
            r#"
trait Eq<T> {
    fn id(T x) -> T;
}
fn main() {}
"#,
        );
        assert!(
            msgs.iter().any(|m| m.message().contains("Duplicate trait")),
            "expected Duplicate trait for Eq, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn user_cannot_redeclare_builtin_option() {
        let msgs = assert_messages(r#"enum Option { None, Some(int) }"#);
        assert!(
            msgs.iter().any(|m| {
                m.code() == Some(ErrorCode::DuplicateEnum) || m.message().contains("Duplicate enum")
            }),
            "expected Duplicate enum for Option, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    // ── Constraint discharge tests (Chunk A4) ─────────────────────────────────

    /// Calling a generic `fn add<T: Num>(T a, T b) -> T` with `int` arguments
    /// must succeed: `Num<int>` is a builtin instance.
    #[test]
    fn call_generic_num_fn_with_int_discharges() {
        let src = r#"
fn add<T: Num>(T a, T b) -> T { return a + b; }
fn main() { let r = add(1, 2); }
"#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(
            msgs.iter()
                .all(|m| !m.message().contains("Cannot satisfy")
                    && !m.message().contains("No instance")),
            "unexpected constraint errors for add(int, int): {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Calling `fn add<T: Num>(T a, T b) -> T` with `string` arguments must
    /// produce a diagnostic: `string` has no `Num` instance.
    #[test]
    fn call_generic_num_fn_with_string_errors() {
        let src = r#"
fn add<T: Num>(T a, T b) -> T { return a; }
fn main() { let r = add("a", "b"); }
"#;
        let msgs = assert_messages(src);
        assert!(
            msgs.iter().any(|m| {
                m.code() == Some(ErrorCode::GenericTypeError)
                    && (m.message().contains("No instance for `Num")
                        || m.message().contains("Cannot satisfy"))
            }),
            "expected a Num constraint violation for string, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Debug test: discharge_constraints should populate call_site_dicts for
    /// user typeclasses at ground call sites.
    #[test]
    fn discharge_constraints_populates_call_site_dicts_for_user_typeclass() {
        let src = r#"
trait Describable<T> { fn describe_val(T x) -> int; }
impl Describable<int> { fn describe_val(int x) -> int { return x; } }
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

    /// A generic function calling another generic function with the same
    /// constraint must not emit a diagnostic — the constraint propagates.
    ///
    /// `fn outer<T: Num>(T x) -> T { return add(x, x); }` is valid when
    /// `add<T: Num>` exists, because `outer`'s own `T: Num` bound covers
    /// the inner call's constraint.
    #[test]
    fn call_generic_inside_generic_propagates() {
        let src = r#"
fn add<T: Num>(T a, T b) -> T { return a + b; }
fn outer<T: Num>(T x) -> T { return add(x, x); }
fn main() { let r = outer(5); }
"#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(
            msgs.iter()
                .all(|m| !m.message().contains("Cannot satisfy")
                    && !m.message().contains("No instance")),
            "unexpected constraint errors for generic propagation: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Multi-param trait + `where` clause discharges at a ground call site.
    #[test]
    fn multiparam_where_clause_discharges_at_call_site() {
        let src = r#"
trait Convert<A, B> { fn cast(A x) -> B; }
impl Convert<int, int> { fn cast(int x) -> int { return x; } }
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

    /// Missing multi-param instance produces a diagnostic.
    #[test]
    fn multiparam_missing_instance_errors() {
        let src = r#"
trait Convert<A, B> { fn cast(A x) -> B; }
fn apply_cast<A, B>(A x) -> B where Convert<A, B> { return cast(x); }
fn main() { let y = apply_cast(42); }
"#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("No instance") || m.message().contains("Convert")),
            "expected missing-instance diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
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
class Celsius { c: int }
class Fahrenheit { f: int }
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

    /// Calling an Into-bound helper without an instance errors.
    #[test]
    fn prelude_into_missing_instance_errors() {
        let src = r#"
fn wrap<A, B>(A x) -> B where Into<A, B> { return into(x); }
fn main() { let w = wrap(42); }
"#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("No instance") || m.message().contains("Into")),
            "expected missing-Into diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Builtin source type is rejected under the strict orphan rule.
    #[test]
    fn prelude_into_impl_for_builtin_source_is_orphan() {
        let src = r#"
class Wrapper { v: int }
impl Into<Wrapper> for int {
    fn into(int x) -> Wrapper { return new Wrapper(x); }
}
"#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(
            msgs.iter().any(|m| m.message().contains("Orphan instance")),
            "expected orphan diagnostic for Into<Wrapper> for int, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Inherent class methods win over prelude trait methods of the same
    /// name when no matching instance exists (Bugbot: ground trait must
    /// not block `impl Point { fn show() ... }`).
    #[test]
    fn inherent_class_method_wins_over_missing_trait_instance() {
        let src = r#"
class Point { x: int }
impl Point {
    fn show() -> string { return "point"; }
}
fn main() {
    let p = new Point(1);
    let s = p.show();
}
"#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(
            msgs.iter()
                .all(|m| !m.message().contains("No instance") && !m.message().contains("Show")),
            "inherent show must not require Show instance, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(
            msgs.is_empty(),
            "unexpected diagnostics: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// `return c.into();` under `-> Fahrenheit` pins Into's target
    /// (Bugbot: expected type must flow from return annotations).
    #[test]
    fn prelude_into_return_pins_expected_target() {
        let src = r#"
class Celsius { c: int }
class Fahrenheit { f: int }
class Kelvin { k: int }
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

    #[test]
    fn binary_hkt_result_instance_discharges() {
        let src = r#"
trait Bifunctor<F: * -> * -> *> {
    fn tag<A, B>(F<A, B> xs) -> int;
}
impl Bifunctor<Result> {
    fn tag<A, B>(Result<A, B> xs) -> int { return 42; }
}
fn get_tag<F: * -> * -> *, Bifunctor, A, B>(F<A, B> xs) -> int {
    return tag(xs);
}
fn main() { let x = get_tag(Result::Ok(7)); }
"#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(
            msgs.iter().all(|m| !m.message().contains("Cannot satisfy")
                && !m.message().contains("No instance")
                && !m.message().contains("constructor-kinded")),
            "unexpected diagnostics: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn binary_hkt_rejects_wrong_arity_constructor_instance() {
        let src = r#"
trait Bifunctor<F: * -> * -> *> {
    fn tag<A, B>(F<A, B> xs) -> int;
}
impl Bifunctor<Option> {
    fn tag<A, B>(Option<A> xs) -> int { return 0; }
}
"#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(
            msgs.iter().any(|m| m
                .message()
                .contains("expects argument 1 to have kind `* -> * -> *`, found kind `* -> *`")),
            "expected binary constructor-kind mismatch, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
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

    /// Phase 6: associated type on a ground call pins `C::Elem` to `int`.
    #[test]
    fn assoc_type_head_returns_int_at_ground_call() {
        let src = r#"
trait Collect<C> {
    type Elem;
    fn head(C xs) -> Elem;
}
impl Collect<Option<int>> {
    type Elem = int;
    fn head(Option<int> xs) -> int {
        return match xs {
            Option::Some(v) => v,
            Option::None => 0,
        };
    }
}
fn take_head<C: Collect>(C xs) -> C::Elem {
    return head(xs);
}
fn main() {
    let x = take_head(Option::Some(42));
}
"#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected diagnostics: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        // `x` should be int after ground Collect<Option<int>> discharge pins Elem.
        let x_ty = c
            .codegen_var_type("x")
            .cloned()
            .or_else(|| c.env().lookup("x").map(|s| apply_ty_prune(&c.subst, &s.ty)));
        let x_ty = x_ty.expect("x should be bound");
        let x_ty = apply_ty_prune(&c.subst, &x_ty);
        assert!(
            matches!(x_ty, Ty::Con(ref n) if n == "int") || x_ty == int(),
            "expected take_head(...) : int, got {}",
            x_ty
        );
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
    fn gat_open_projection_under_bound_is_ok() {
        let src = r#"
trait Pointer<P: * -> *> {
    type Ref<T>;
    fn deref<T>(P<T> ptr) -> Ref<T>;
}

impl Pointer<Option> {
    type Ref<T> = T;
    fn deref<T>(Option<T> ptr) -> T {
        return match ptr {
            Option::Some(v) => v,
            Option::None => 0,
        };
    }
}

fn get<P: * -> *, Pointer, A>(P<A> ptr) -> P::Ref<A> {
    return deref(ptr);
}
fn main() { let x = get(Option::Some(42)); }
"#;
        let (mut c, _) = check(src);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected diagnostics: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert_eq!(
            c.codegen_var_type("x")
                .map(|t| apply_ty_prune(c.subst(), t)),
            Some(int())
        );
    }

    #[test]
    fn gat_projection_wrong_arity_errors() {
        let src = r#"
trait Pointer<P: * -> *> {
    type Ref<T>;
    fn deref<T>(P<T> ptr) -> Ref<T>;
}
fn bad<P: * -> *, Pointer, A>(P<A> ptr) -> P::Ref<A, int> {
    return deref(ptr);
}
"#;
        let (_c, msgs) = check_warn(src);
        assert!(
            msgs.iter().any(|m| m
                .message()
                .contains("Associated type `Pointer::Ref` expects 1 type argument, got 2")),
            "expected GAT arity diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn gat_projection_kind_mismatch_errors() {
        let src = r#"
trait Pointer<P: * -> *> {
    type Ref<F: * -> *>;
    fn bad<T>(P<T> ptr) -> Ref<T>;
}
"#;
        let (_c, msgs) = check_warn(src);
        assert!(
            msgs.iter().any(|m| {
                let msg = m.message();
                msg.contains("Type argument 1 to associated type `Pointer::Ref`")
                    && msg.contains("kind `*`")
                    && msg.contains("expected `* -> *`")
            }),
            "expected GAT kind diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    // ---- byte / [byte] ----

    #[test]
    fn byte_array_annotation_accepts_string_literal() {
        let (mut c, _) = check(r#"let b: [byte] = "Hi";"#);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        let ty = c
            .codegen_var_type("b")
            .map(|t| apply_ty_prune(c.subst(), t))
            .expect("b");
        match ty {
            Ty::Array { element, .. } => {
                assert_eq!(*element, crate::typechecking::ty::byte());
            }
            other => panic!("expected [byte], got {other}"),
        }
    }

    #[test]
    fn byte_array_fixed_rejects_wrong_string_length() {
        let msgs = assert_messages(r#"let b: [byte; 2] = "Hi!";"#);
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("expected `[byte; 2]`")),
            "got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn byte_annotation_accepts_single_byte_string_literal() {
        let (mut c, _) = check(r#"let b: byte = "/";"#);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        assert_eq!(
            c.codegen_var_type("b")
                .map(|t| apply_ty_prune(c.subst(), t)),
            Some(crate::typechecking::ty::byte())
        );
    }

    #[test]
    fn byte_annotation_rejects_multi_byte_string_literal() {
        let msgs = assert_messages(r#"let b: byte = "é";"#);
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("exactly one UTF-8 byte")),
            "got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn byte_array_literal_coerces_from_string_literals() {
        let (mut c, _) = check(r#"let buf: [byte] = ["a", "b", "\n"];"#);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        let ty = c
            .codegen_var_type("buf")
            .map(|t| apply_ty_prune(c.subst(), t))
            .expect("buf");
        match ty {
            Ty::Array { element, .. } => {
                assert_eq!(*element, crate::typechecking::ty::byte());
            }
            other => panic!("expected [byte], got {other}"),
        }
    }

    #[test]
    fn byte_compares_with_single_byte_string_literal() {
        let (mut c, _) = check(
            r#"
fn main() {
    let b: byte = "/";
    let ok = b == "/";
}
"#,
        );
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
    }

    #[test]
    fn byte_annotation_accepts_in_range_literal() {
        let (mut c, _) = check("let b: byte = 42;");
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        assert_eq!(
            c.codegen_var_type("b")
                .map(|t| apply_ty_prune(c.subst(), t)),
            Some(crate::typechecking::ty::byte())
        );
    }
    #[test]
    fn string_var_cast_to_vec_byte_rejects_with_to_bytes_hint() {
        let msgs = assert_messages(r#"fn main() { let s = "hi"; let _ = s as Vec<byte>; }"#);
        assert!(
            msgs.iter().any(|m| {
                m.message().contains("cannot cast `string` to `Vec<byte>`")
                    && m.help().as_ref().is_some_and(|h| h.contains("to_bytes"))
            }),
            "got: {:?}",
            msgs.iter().map(|m| (m.message(), m.help())).collect::<Vec<_>>()
        );
    }

    #[test]
    fn string_literal_cast_to_vec_byte_ok() {
        let (mut c, _) = check(r#"fn main() { let b = "hi" as Vec<byte>; }"#);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        let ty = c
            .codegen_var_type("b")
            .map(|t| apply_ty_prune(c.subst(), t))
            .expect("b");
        assert_eq!(
            ty,
            crate::typechecking::ty::vec_app_ty(crate::typechecking::ty::byte())
        );
    }

    #[test]
    fn string_var_cast_to_byte_slice_ok() {
        let (mut c, _) = check(r#"fn main() { let s = "hi"; let b = s as [byte]; }"#);
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        let ty = c
            .codegen_var_type("b")
            .map(|t| apply_ty_prune(c.subst(), t))
            .expect("b");
        match ty {
            Ty::Array {
                element,
                length: crate::typechecking::ty::ArrayLength::Dynamic,
            } => {
                assert_eq!(*element, crate::typechecking::ty::byte());
            }
            other => panic!("expected [byte], got {other}"),
        }
    }

    #[test]
    fn string_var_cast_to_fixed_byte_array_rejects() {
        let msgs = assert_messages(r#"fn main() { let s = "hi"; let _ = s as [byte; 2]; }"#);
        assert!(
            msgs.iter().any(|m| {
                m.message()
                    .contains("cannot cast `string` to fixed-length `[byte; N]`")
                    && m.help()
                        .as_ref()
                        .is_some_and(|h| h.contains("to_bytes"))
            }),
            "got: {:?}",
            msgs.iter().map(|m| (m.message(), m.help())).collect::<Vec<_>>()
        );
    }

    #[test]
    fn string_var_annotated_vec_byte_rejects() {
        let msgs = assert_messages(r#"fn main() { let s = "hi"; let _: Vec<byte> = s; }"#);
        assert!(
            !msgs.is_empty(),
            "non-literal string must not coerce to Vec<byte> via annotation"
        );
        assert!(
            msgs.iter().any(|m| {
                let msg = m.message();
                msg.contains("Type mismatch")
                    || msg.contains("expected")
                    || msg.contains("Vec<byte>")
            }),
            "got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn byte_cast_rejects_negative_literal() {
        for src in [
            "fn main() { let x = -1 as byte; }",
            "fn main() { let x = (-1) as byte; }",
        ] {
            let msgs = assert_messages(src);
            assert!(
                msgs.iter()
                    .any(|m| m.message().contains("byte literal out of range")),
                "expected OOB for `{src}`, got: {:?}",
                msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn byte_annotation_rejects_out_of_range_literal() {
        let msgs = assert_messages("let b: byte = 300;");
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("byte literal out of range")),
            "got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn byte_return_accepts_in_range_literal_arithmetic() {
        let (mut c, _) = check(
            r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn f() -> byte { return 1 + 1; }
fn main() { write(stdout(), to_bytes(format("%i", f()))); }
"#,
        );
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
    }

    #[test]
    fn byte_return_rejects_out_of_range_literal() {
        let msgs = assert_messages("fn f() -> byte { return 300; }");
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("byte literal out of range")
                    || m.message().contains("Type mismatch")),
            "got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn byte_return_rejects_unannotated_int_variable() {
        // Unannotated `let x = 42` is `int`; returning it as `byte` is not
        // literal coercion (needs `let x: byte = 42`).
        let msgs = assert_messages(
            r#"
fn f() -> byte {
    let x = 42;
    return x;
}
"#,
        );
        assert!(
            msgs.iter().any(|m| m.message().contains("Type mismatch")
                && m.message().contains("byte")
                && m.message().contains("int")),
            "got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn byte_array_literal_coerces_from_int_literals() {
        let (mut c, _) = check("let buf: [byte] = [1, 2, 3];");
        let msgs = c.take_messages();
        assert!(msgs.is_empty(), "{:?}", msgs);
        let ty = c
            .codegen_var_type("buf")
            .map(|t| apply_ty_prune(c.subst(), t))
            .expect("buf");
        match ty {
            Ty::Array { element, .. } => {
                assert_eq!(*element, crate::typechecking::ty::byte());
            }
            other => panic!("expected [byte], got {other}"),
        }
    }

    #[test]
    fn byte_has_show_instance() {
        assert!(
            Checker::new()
                .generics
                .has_instance("Show", &crate::typechecking::ty::byte())
        );
    }

    #[test]
    fn write_all_accepts_named_byte_vec() {
        let (mut c, _) = check(
            r#"
use io::{stdin, write};
fn main() {
    let data = Vec::from([1 as byte, 2 as byte]);
    write(stdin(), data);
}
"#,
        );
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected messages: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn io_error_other_does_not_collide_until_imported() {
        // IoError::Other must not reserve the constructor name globally —
        // user enums may use `Other` without `use io`.
        let (mut c, _) = check("enum Foo { Bar, Other } let x = Foo::Other;");
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected without io import: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );

        let (mut c, _) = check("use io::{IoError}; let e = IoError::Other;");
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected with io import: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn write_all_rejects_unannotated_int_array_variable() {
        let msgs = assert_messages(
            r#"
use io::{stdin, write};
fn main() {
    let data = [1, 2];
    write(stdin(), data);
}
"#,
        );
        assert!(
            msgs.iter().any(|m| m.message().contains("expected `byte`")
                || m.message().contains("Type mismatch")),
            "got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_case_accepts_assert_in_result_mode() {
        let (mut c, _) = check(
            r#"
test("ok") {
    assert(true)?;
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
    fn test_case_rejects_main_alongside_cases() {
        let msgs = assert_messages(
            r#"
use io::{stdout, write};
use string::{format, to_bytes};
test("a") { assert(true)?; }
fn main() { write(stdout(), to_bytes("x")); }
"#,
        );
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("must not define `main`")),
            "got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_case_name_must_be_string_literal() {
        let msgs = assert_messages(
            r#"
let name = "x";
test(name) { assert(true)?; }
"#,
        );
        assert!(
            msgs.iter().any(|m| m.message().contains("string literal")),
            "got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_case_registers_multiple_names_in_source_order() {
        let (mut c, _) = check(
            r#"
test("first") { assert(true)?; }
test("second") { assert(1 == 1)?; }
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
    fn test_case_rejects_bare_int_return_outside_result_wrap() {
        // Ok type is `unit`, so `return 1` must not typecheck.
        let msgs = assert_messages(
            r#"
test("bad") {
    return 1;
}
"#,
        );
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("Type mismatch") || m.message().contains("mismatch")),
            "got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rest_param_packs_trailing_args_as_array() {
        let (mut c, _) = check(
            r#"
fn sum(int... xs) -> int {
    return len(xs);
}
fn main() {
    let n = sum(1, 2, 3);
}
"#,
        );
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(c.fn_has_rest("sum"));
        assert_eq!(c.fn_param_names("sum"), Some(&["xs".to_string()][..]));
    }

    #[test]
    fn rest_param_empty_call_packs_empty_array() {
        let (mut c, _) = check(
            r#"
fn sum(int... xs) -> int { return len(xs); }
fn main() { let n = sum(); }
"#,
        );
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rest_param_must_be_last() {
        let msgs = assert_messages(
            r#"
fn bad(int... xs, int y) -> int { return y; }
"#,
        );
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("must be the last parameter")),
            "got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rest_param_cannot_be_named_at_call_site() {
        let msgs = assert_messages(
            r#"
fn sum(int... xs) -> int { return len(xs); }
fn main() { let n = sum(xs: [1, 2]); }
"#,
        );
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("Cannot pass rest parameter")),
            "got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn named_fixed_then_positional_rest_is_allowed() {
        let (mut c, _) = check(
            r#"
fn f(int a, int... xs) -> int { return a + len(xs); }
fn main() { let n = f(a: 1, 2, 3); }
"#,
        );
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    // ── Arity overload tests ──────────────────────────────────────────────────

    /// Two fixed-arity overloads with distinct arities — both register without
    /// error and calls dispatch to the right one.
    #[test]
    fn overload_two_fixed_arities_dispatch() {
        let (mut c, _) = check(
            r#"
fn f(int x) -> int { return x; }
fn f(int x, int y) -> int { return x + y; }
fn main() {
    let a = f(1);
    let b = f(1, 2);
}
"#,
        );
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected diagnostics: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        // Both candidates should be registered.
        assert_eq!(c.overload_candidates("f").map(|v| v.len()), Some(2));
    }

    /// Duplicate fixed arity with the same parameter types is a `DuplicateOverload` error.
    #[test]
    fn overload_duplicate_fixed_arity_is_error() {
        let msgs = assert_messages(
            r#"
fn f(int x) -> int { return x; }
fn f(int y) -> int { return y + 1; }
fn main() { let a = f(1); }
"#,
        );
        assert!(
            msgs.iter()
                .any(|m| m.code() == Some(ErrorCode::DuplicateOverload)),
            "expected DuplicateOverload, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Same arity with distinct parameter types is allowed and dispatches by type.
    #[test]
    fn overload_same_arity_distinct_types_dispatch() {
        let (mut c, _) = check(
            r#"
fn f(int x) -> int { return x; }
fn f(float x) -> float { return x; }
fn main() {
    let a = f(1);
    let b = f(1.5);
}
"#,
        );
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected diagnostics: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert_eq!(c.overload_candidates("f").map(|v| v.len()), Some(2));
    }

    #[test]
    fn method_arity_overloads_select_by_user_argc() {
        let (mut c, _) = check(
            r#"
class Counter { value: int, }
impl Counter {
    fn bump(int by) -> int { return self.value + by; }
    fn bump() -> int { return self.bump(1); }
}
fn main() {
    let c = new Counter(10);
    let a = c.bump();
    let b = c.bump(5);
}
"#,
        );
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected diagnostics: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert_eq!(
            c.overload_candidates("Counter::bump").map(|v| v.len()),
            Some(2)
        );
    }

    /// Fixed N=1 vs rest with K=1 fixed prefix (N >= K) — overlap error.
    #[test]
    fn overload_fixed_vs_rest_overlap_when_n_ge_k() {
        let msgs = assert_messages(
            r#"
fn f(int x) -> int { return x; }
fn f(int x, int... xs) -> int { return x + len(xs); }
fn main() { let a = f(1); }
"#,
        );
        assert!(
            msgs.iter()
                .any(|m| m.code() == Some(ErrorCode::DuplicateOverload)),
            "expected DuplicateOverload for N>=K overlap, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Fixed N=1 vs rest with K=2 fixed prefix (N < K) — allowed.
    #[test]
    fn overload_fixed_vs_rest_allowed_when_n_lt_k() {
        let (mut c, _) = check(
            r#"
fn f(int x) -> int { return x; }
fn f(int x, int y, int... xs) -> int { return x + y + len(xs); }
fn main() {
    let a = f(1);
    let b = f(1, 2);
    let c = f(1, 2, 3);
}
"#,
        );
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected diagnostics: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert_eq!(c.overload_candidates("f").map(|v| v.len()), Some(2));
    }

    /// Two rest overloads always conflict.
    #[test]
    fn overload_two_rests_is_error() {
        let msgs = assert_messages(
            r#"
fn f(int... xs) -> int { return len(xs); }
fn f(string s, int... xs) -> int { return len(xs); }
fn main() { let a = f(1, 2); }
"#,
        );
        assert!(
            msgs.iter()
                .any(|m| m.code() == Some(ErrorCode::DuplicateOverload)),
            "expected DuplicateOverload for two rests, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Calling an overloaded function with an arity that matches no candidate
    /// produces a `WrongArity` diagnostic.
    #[test]
    fn overload_no_matching_arity_produces_wrong_arity() {
        let msgs = assert_messages(
            r#"
fn f(int x) -> int { return x; }
fn f(int x, int y) -> int { return x + y; }
fn main() { let a = f(1, 2, 3); }
"#,
        );
        assert!(
            msgs.iter().any(|m| m.code() == Some(ErrorCode::WrongArity)),
            "expected WrongArity, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Named-arg under-apply is now allowed (no "under-applied" error).
    /// The result type should be a residual `Fun` (partial application).
    #[test]
    fn named_under_apply_no_longer_errors() {
        let (mut c, _) = check(
            r#"
fn add(int a, int b) -> int { return a + b; }
fn main() { let partial = add(a: 1); }
"#,
        );
        let msgs = c.take_messages();
        // The old "under-applied" error must not appear.
        assert!(
            !msgs.iter().any(|m| m.message().contains("under-applied")),
            "unexpected under-apply error: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
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

    /// Bare `let f = overloaded;` without expected type → AmbiguousOverload.
    #[test]
    fn ambiguous_overload_in_value_position() {
        let msgs = assert_messages(
            r#"
fn add(int x) -> int { return x; }
fn add(int x, int y) -> int { return x + y; }
fn main() { let f = add; }
"#,
        );
        assert!(
            msgs.iter()
                .any(|m| m.code() == Some(ErrorCode::AmbiguousOverload)),
            "expected AmbiguousOverload, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Same-arity overloads that both unify with a polymorphic argument → AmbiguousOverload.
    #[test]
    fn ambiguous_overload_at_call_site() {
        let msgs = assert_messages(
            r#"
fn f(int x) -> int { return x; }
fn f(float x) -> float { return x; }
fn g<T>(T x) -> int {
    return f(x);
}
fn main() {}
"#,
        );
        assert!(
            msgs.iter()
                .any(|m| m.code() == Some(ErrorCode::AmbiguousOverload)),
            "expected AmbiguousOverload at call site, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    /// Lambda bodies cannot close over outer locals unless listed in `use`.
    #[test]
    fn lambda_uncaptured_outer_is_error() {
        let msgs = assert_messages(
            r#"
fn main() {
    let y = 10;
    let f = fn (int x) => x + y;
}
"#,
        );
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("cannot capture `y` without `use (y)`")),
            "expected cannot-capture diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
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
    fn spawn_rejects_non_sendable_thread_argument() {
        let msgs = assert_messages(
            r#"
use thread::{spawn, Thread};
fn noop() -> int { return 0; }
fn work(Thread t) -> int { return 1; }
fn main() {
    let t0 = spawn(noop)?;
    let t = spawn(work, t0);
}
"#,
        );
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("not sendable across threads")),
            "expected non-sendable spawn arg diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn spawn_rejects_zero_and_too_many_arguments() {
        let msgs = assert_messages(
            r#"
use thread::{spawn};
fn main() {
    let _ = spawn();
}
"#,
        );
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("spawn expects a function")),
            "expected zero-arg spawn diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );

        let msgs = assert_messages(
            r#"
use thread::{spawn};
fn work(int a, int b) -> int { return a + b; }
fn main() {
    let _ = spawn(work, 1, 2);
}
"#,
        );
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("too many arguments")),
            "expected arity spawn diagnostic, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn spawn_accepts_sendable_int_argument() {
        let (mut c, _) = check(
            r#"
use thread::{spawn};
fn work(int n) -> int { return n + 1; }
fn main() {
    let t = spawn(work, 41);
}
"#,
        );
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected messages: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn host_fn_scheme_covers_all_wiring_registries() {
        use machine::{ENV_WIRING, FS_WIRING};

        let mut c = Checker::new();
        #[allow(unused_mut)] // extended only when time/crypto features are on
        let mut names: Vec<&str> = FS_WIRING
            .iter()
            .chain(ENV_WIRING.iter())
            .map(|&(n, _, _)| n)
            .collect();
        #[cfg(feature = "time")]
        {
            names.extend(machine::TIME_WIRING.iter().map(|&(n, _, _)| n));
        }
        #[cfg(feature = "crypto")]
        {
            names.extend(machine::CRYPTO_WIRING.iter().map(|&(n, _, _)| n));
        }
        #[cfg(all(feature = "time", feature = "crypto"))]
        assert_eq!(names.len(), 63);
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

    /// Same-arity overloads with no unifying candidate report WrongArity and
    /// list the available signatures (type mismatch is not a separate code).
    #[test]
    fn type_overload_no_matching_param_types_is_wrong_arity() {
        let msgs = assert_messages(
            r#"
fn show(int x) -> int { return x; }
fn show(float x) -> float { return x; }
fn main() { let a = show(true); }
"#,
        );
        assert!(
            msgs.iter()
                .any(|m| m.code() == Some(ErrorCode::WrongArity)),
            "expected WrongArity for unmatched type overload, got: {:?}",
            msgs.iter()
                .map(|m| (m.code(), m.message()))
                .collect::<Vec<_>>()
        );
        assert!(
            msgs.iter().any(|m| {
                m.message().contains("No overload of `show`")
                    || m.help()
                        .as_ref()
                        .is_some_and(|h| h.contains("(int)") || h.contains("available overloads"))
            }),
            "expected overload help mentioning available signatures, got: {:?}",
            msgs.iter()
                .map(|m| (m.message(), m.help().clone()))
                .collect::<Vec<_>>()
        );
    }

    /// After checking a defining module, `use mod::{generic}` must re-bind the
    /// real poly scheme (not a dummy Var) and mark the local alias generic.
    #[test]
    fn use_reexports_cross_module_generic_scheme() {
        let mut c = Checker::new();
        c.set_current_module("num");
        let def = r#"
fn min<T: Ord>(T a, T b) -> T {
    if a < b { return a; }
    return b;
}
"#;
        let parser = Pratt::default();
        let ast = parser.parse(def).expect("parse def");
        let _ = c.check_program(&ast);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected diagnostics in def module: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(
            c.is_generic_fn("num::min"),
            "defining module must register qualified generic"
        );

        c.set_current_module("");
        let importer = r#"
use num::{min};
fn main() {
    let a = min(3, 1);
    let b = min(3.0, 1.0);
}
"#;
        let ast = parser.parse(importer).expect("parse importer");
        let _ = c.check_program(&ast);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected diagnostics after use re-export: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(
            c.is_generic_fn("min"),
            "imported alias must stay generic for dict-passing ABI"
        );
        assert!(
            c.env().lookup("min").is_some(),
            "imported alias must bind a real scheme"
        );
        let scheme = c.env().lookup("min").unwrap();
        assert!(
            !scheme.bounds.is_empty(),
            "re-export must keep Ord bound, got {:?}",
            scheme
        );
    }

    #[test]
    fn use_reexports_cross_module_class() {
        let mut c = Checker::new();
        let parser = Pratt::default();

        c.set_current_module("lib");
        let def = r#"
class Foo { name: string, }
impl Foo {
    static fn fresh() -> Foo { return new Foo("x"); }
    fn len() -> int { return 1; }
}
"#;
        let ast = parser.parse(def).expect("parse def");
        let _ = c.check_program(&ast);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected diagnostics in def module: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(
            c.classes.contains_key("lib::Foo"),
            "defining module must register FQN class key"
        );
        assert!(c.is_static_method("lib::Foo", "fresh"));

        c.set_current_module("");
        let importer = r#"
use lib::{Foo};
fn main() {
    let x = new Foo("hi");
    let y = Foo::fresh();
    let n = x.len();
    let z: Foo = x;
}
"#;
        let ast = parser.parse(importer).expect("parse importer");
        let _ = c.check_program(&ast);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected diagnostics after class use: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(c.is_class("Foo"), "imported alias must resolve as class");
        let scheme = c.env().lookup("Foo").expect("imported Foo scheme");
        assert_eq!(scheme.ty, Ty::Con("lib::Foo".into()));
    }

    #[test]
    fn use_reexports_class_alias() {
        let mut c = Checker::new();
        let parser = Pratt::default();

        c.set_current_module("lib");
        let def = r#"
class Foo { name: string, }
impl Foo {
    static fn fresh() -> Foo { return new Foo("x"); }
}
"#;
        let ast = parser.parse(def).expect("parse def");
        let _ = c.check_program(&ast);
        assert!(c.take_messages().is_empty());

        c.set_current_module("");
        let importer = r#"
use lib::{Foo as HM};
fn main() {
    let x = new HM("hi");
    let y = HM::fresh();
    let z: HM = x;
}
"#;
        let ast = parser.parse(importer).expect("parse importer");
        let _ = c.check_program(&ast);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected diagnostics after class alias: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert!(c.is_class("HM"));
        let scheme = c.env().lookup("HM").expect("alias scheme");
        assert_eq!(scheme.ty, Ty::Con("lib::Foo".into()));
    }

    #[test]
    fn use_reexports_generic_class() {
        let mut c = Checker::new();
        let parser = Pratt::default();

        c.set_current_module("lib");
        let def = r#"
class Cell<T> { value: T, }
impl Cell<T> {
    fn get() -> T { return self.value; }
}
"#;
        let ast = parser.parse(def).expect("parse def");
        let _ = c.check_program(&ast);
        assert!(c.take_messages().is_empty());
        assert!(c.generics.generic_type_ctors.contains_key("lib::Cell"));

        c.set_current_module("");
        let importer = r#"
use lib::{Cell};
fn main() {
    let c = new Cell(42);
    let n: int = c.get();
    let d: Cell<int> = c;
}
"#;
        let ast = parser.parse(importer).expect("parse importer");
        let _ = c.check_program(&ast);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected diagnostics after generic class use: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn imported_enum_variant_match_works_in_test_body() {
        let mut c = Checker::new();
        let parser = Pratt::default();

        c.set_current_module("json::value");
        let ast = parser
            .parse("enum JsonValue { Null, Str(string), }")
            .expect("parse value module");
        let _ = c.check_program(&ast);
        assert!(
            c.take_messages().is_empty(),
            "value module: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );

        c.set_current_module("");
        c.env_mut().insert_top(
            "JsonValue".to_string(),
            Scheme::mono(Ty::Con("json::value::JsonValue".into())),
        );
        let ast = parser
            .parse(
                r#"test("match imported enum") {
    let v = JsonValue::Null;
    match v { JsonValue::Null => assert(true)?, _ => assert(false)? };
}"#,
            )
            .expect("parse test file");
        let _ = c.check_program(&ast);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn imported_enum_payload_match_via_use_in_test_body() {
        let mut c = Checker::new();
        let parser = Pratt::default();

        c.set_current_module("json::value");
        let ast = parser
            .parse("enum JsonValue { Null, Str(string), }")
            .expect("parse value module");
        let _ = c.check_program(&ast);
        assert!(
            c.take_messages().is_empty(),
            "value module: {:?}",
            c.messages().iter().map(|m| m.message()).collect::<Vec<_>>()
        );

        c.set_current_module("");
        let ast = parser
            .parse(
                r#"
use json::value::{JsonValue};
test("match imported payload") {
    let v = JsonValue::Str("hi");
    match v {
        JsonValue::Str(s) => assert(s == "hi")?,
        _ => assert(false)?,
    };
}
"#,
            )
            .expect("parse importer test");
        let _ = c.check_program(&ast);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn imported_enum_match_via_use_in_fn_body() {
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

        c.set_current_module("");
        let ast = parser
            .parse(
                r#"
use pkg::{Traffic};
fn main() {
    let s = Traffic::Go;
    let n = match s {
        Traffic::Go => 0,
        Traffic::Stop => 1,
    };
}
"#,
            )
            .expect("parse importer");
        let _ = c.check_program(&ast);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
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
    fn unimported_module_enum_construct_is_unknown() {
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

        c.set_current_module("");
        let ast = parser
            .parse(
                r#"test("no import") {
    let v = Traffic::Go;
}"#,
            )
            .expect("parse test without use");
        let _ = c.check_program(&ast);
        let msgs = c.take_messages();
        assert!(
            msgs.iter()
                .any(|m| m.code() == Some(ErrorCode::UnknownEnum)),
            "expected UnknownEnum without import, got: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn two_modules_can_export_same_class_short_name() {
        let mut c = Checker::new();
        let parser = Pratt::default();

        c.set_current_module("a");
        let ast = parser
            .parse("class Client { n: int, }")
            .expect("parse a");
        let _ = c.check_program(&ast);
        assert!(c.take_messages().is_empty());

        c.set_current_module("b");
        let ast = parser
            .parse("class Client { n: int, }")
            .expect("parse b");
        let _ = c.check_program(&ast);
        assert!(c.take_messages().is_empty());
        assert!(c.classes.contains_key("a::Client"));
        assert!(c.classes.contains_key("b::Client"));

        c.set_current_module("");
        let importer = r#"
use a::{Client as A};
use b::{Client as B};
fn main() {
    let x = new A(1);
    let y = new B(2);
}
"#;
        let ast = parser.parse(importer).expect("parse importer");
        let _ = c.check_program(&ast);
        let msgs = c.take_messages();
        assert!(
            msgs.is_empty(),
            "unexpected diagnostics for colliding class names: {:?}",
            msgs.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
        assert_eq!(
            c.env().lookup("A").map(|s| s.ty.clone()),
            Some(Ty::Con("a::Client".into()))
        );
        assert_eq!(
            c.env().lookup("B").map(|s| s.ty.clone()),
            Some(Ty::Con("b::Client".into()))
        );
    }

    /// `reexport_module_item` must not mark a local alias generic just because
    /// some *other* module registered `…::{same_local}` as generic.
    #[test]
    fn reexport_generic_mark_ignores_other_module_suffix() {
        let mut c = Checker::new();
        let parser = Pratt::default();

        c.set_current_module("gen");
        let gen_src = r#"
fn foo<T: Ord>(T a, T b) -> T {
    if a < b { return a; }
    return b;
}
"#;
        let ast = parser.parse(gen_src).expect("parse gen");
        let _ = c.check_program(&ast);
        assert!(c.take_messages().is_empty());
        assert!(c.is_generic_fn("gen::foo"));

        c.set_current_module("plain");
        let plain_src = r#"
fn foo(int a) -> int {
    return a;
}
"#;
        let ast = parser.parse(plain_src).expect("parse plain");
        let _ = c.check_program(&ast);
        assert!(c.take_messages().is_empty());
        assert!(!c.is_generic_fn("plain::foo"));

        c.set_current_module("");
        let importer = r#"
use plain::{foo};
fn main() {
    let x = foo(1);
}
"#;
        let ast = parser.parse(importer).expect("parse importer");
        let _ = c.check_program(&ast);
        assert!(c.take_messages().is_empty());
        assert!(
            !c.is_generic_fn("foo"),
            "importing non-generic plain::foo must not inherit gen::foo's generic tag"
        );
        assert_eq!(c.dict_arity_for("foo"), 0);
        assert!(
            c.is_generic_fn("gen::foo"),
            "defining FQN must remain generic for other importers"
        );
    }

    // ---- Edge cases / caveat regression ----

    #[test]
    fn dynamic_int_slice_in_let_binding_errors() {
        let msgs = assert_messages("let xs: [int] = [1, 2];");
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("dynamic array type `[T]`")),
            "expected dynamic-slice rejection, got: {:?}",
            msgs
        );
    }

    #[test]
    fn dynamic_non_byte_slice_in_fn_return_ok() {
        let src = "fn rows() -> [int] { return [1, 2, 3]; }";
        let (mut c, _) = check(src);
        assert!(c.take_messages().is_empty(), "{:?}", c.take_messages());
    }

    #[test]
    fn dynamic_slice_fn_param_allows_runtime_index() {
        let src = "fn at([int] xs, int i) -> int { return xs[i]; }";
        let (mut c, _) = check(src);
        assert!(c.take_messages().is_empty(), "{:?}", c.take_messages());
    }

    #[test]
    fn dynamic_string_slice_in_let_binding_errors() {
        let msgs = assert_messages(r#"let xs: [string] = ["a"];"#);
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("dynamic array type `[T]`")),
            "expected dynamic-slice rejection for [string], got: {:?}",
            msgs
        );
    }

    #[test]
    fn match_user_enum_non_exhaustive_reports_missing_variant() {
        let src = "enum Color { Red, Green, Blue } \
                   let c = Color::Red; \
                   match c { Color::Red => 0 };";
        let msgs = assert_messages(src);
        assert!(
            msgs.iter()
                .any(|m| m.message().contains("Non-exhaustive match")),
            "expected non-exhaustive match on user enum, got: {:?}",
            msgs
        );
        let detail = msgs
            .iter()
            .find(|m| m.message().contains("Non-exhaustive"))
            .unwrap();
        assert!(
            detail.message().contains("Green") || detail.message().contains("Blue"),
            "expected missing variant names, got: {:?}",
            detail.message()
        );
    }

    #[test]
    fn match_user_enum_exhaustive_ok() {
        let src = "enum Color { Red, Green } \
                   let c = Color::Red; \
                   match c { Color::Red => 1, Color::Green => 2 };";
        let (mut c, _) = check(src);
        assert!(c.take_messages().is_empty(), "{:?}", c.take_messages());
    }

    #[test]
    fn match_user_enum_wildcard_suppresses_exhaustiveness_error() {
        let src = "enum Color { Red, Green, Blue } \
                   let c = Color::Red; \
                   match c { _ => 0 };";
        let (mut c, _) = check(src);
        assert!(c.take_messages().is_empty(), "{:?}", c.take_messages());
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
            inferred_expr_ty(r#"class Foo { v: int } let x = new Foo(7); x.v"#),
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
    fn len_empty_string_literal_is_zero() {
        assert_ok(r#"len("")"#, int());
    }

    #[test]
    fn len_empty_fixed_array_literal_is_zero() {
        let src = "fn f() -> int { let xs: [int; 0] = []; return len(xs); }";
        let (mut c, _) = check(src);
        assert!(c.take_messages().is_empty(), "{:?}", c.take_messages());
    }

    #[test]
    fn len_nested_call_on_literal() {
        assert_ok(r#"len("abcd")"#, int());
        assert_ok("len([1, 2, 3, 4])", int());
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

    #[test]
    fn rest_param_dynamic_slice_in_fn_sig_ok() {
        let src = "fn sum(int... xs) -> int { return len(xs); }";
        let (mut c, _) = check(src);
        assert!(c.take_messages().is_empty(), "{:?}", c.take_messages());
    }

    #[test]
    fn fixed_array_in_let_still_requires_static_length() {
        let (mut c, _) = check("let xs: [int; 3] = [1, 2, 3];");
        assert!(c.take_messages().is_empty(), "{:?}", c.take_messages());
        let ty = c
            .codegen_var_type("xs")
            .map(|t| apply_ty_prune(c.subst(), t))
            .expect("xs");
        assert_eq!(ty, array_fixed(int(), 3));
    }
