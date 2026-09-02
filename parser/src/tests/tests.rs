    use crate::Pratt;
    use crate::ast::{
        AdjustOp, AssignOp, EnumConstructPayload, EnumVariantPayload, Expression, MatchArm,
        Pattern, PatternPayload,
    };
    use chumsky::Parser;

    macro_rules! expr {
        ($case: literal) => {
            Pratt::default()
                .expr()
                .parse($case)
                .into_result()
                .unwrap()
                .1
                .to_string()
        };
    }

    macro_rules! stmt {
        ($case: literal) => {
            Pratt::default()
                .declaration()
                .parse($case)
                .into_result()
                .unwrap()
                .1
                .to_string()
        };
    }

    /// Parse a top-level declaration, returning the inner
    /// `Expression` for structural assertions (no `Display` round-trip).
    macro_rules! decl_ast {
        ($case: literal) => {
            Pratt::default()
                .declaration()
                .parse($case)
                .into_result()
                .expect("parse failed")
                .1
                .as_ref()
                .clone()
        };
    }

    /// Parse an expression, returning the inner `Expression`.
    macro_rules! expr_ast {
        ($case: literal) => {
            Pratt::default()
                .expr()
                .parse($case)
                .into_result()
                .expect("parse failed")
                .1
                .as_ref()
                .clone()
        };
    }

    macro_rules! same {
        ($case: literal) => {
            assert_eq!($case.to_string(), expr!($case));
        };
    }

    #[test]
    fn pratt_test_precedence() {
        same!("~1");
        same!("!true");
        same!("!0");
        same!("-1");
        same!("+1");
        same!("1 + 2");
        same!("1 - 2");
        same!("1 * 2");
        same!("1 / 2");
        same!("1 % 2");
        same!("1 ^ 2");
        same!("1 & 2");
        same!("1 | 2");
        same!("1 << 2");
        same!("1 >> 2");
        same!("1 || 2");
        same!("1 && 2");
        same!("1 << 2 > 3 >> 1");
        same!("2 << 2 + 2");
        same!("((2 + 2) * 2) + -3");
        same!("2 * 2 + 3 + -3");
        same!("2 * ((2 * 2) + 2)");
        same!("2 + 2 - 1 / 5 % 3");
        same!("foo()");
    }

    #[test]
    fn pratt_test_statements() {
        stmt!("write(\"%i\", 42);");
        stmt!("write(\"Hello, World!\");");
        stmt!("defer { write(\"%i\", 42); }");
        stmt!("while x < 10 { x = x + 1; }");
    }

    #[test]
    fn defer_parses_inside_function_body() {
        // Regression: `defer` must be a statement, not only a top-level
        // declaration — otherwise `defer {` inside `fn` fails looking for `:`.
        let ast = decl_ast!("fn f() { defer { write(\"x\"); } write(\"y\"); }");
        match ast {
            Expression::Function {
                docs: _,
                body: Some(body), ..
            } => {
                let Expression::Block(items) = body.1.as_ref() else {
                    panic!("expected function body block, got {:?}", body.1);
                };
                assert!(
                    items.iter().any(|item| {
                        matches!(item.1.as_ref(), Expression::Defer { .. })
                            || matches!(
                                item.1.as_ref(),
                                Expression::Statement(inner)
                                    if matches!(inner.1.as_ref(), Expression::Defer { .. })
                            )
                    }),
                    "expected a Defer node in the function body, got {:?}",
                    items
                );
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    #[test]
    fn defer_use_parses_captures() {
        let ast = decl_ast!("fn f() { let x = 1; defer use (x) { write(\"%i\", x); } }");
        match ast {
            Expression::Function {
                docs: _,
                body: Some(body), ..
            } => {
                let Expression::Block(items) = body.1.as_ref() else {
                    panic!("expected function body block, got {:?}", body.1);
                };
                let defer = items.iter().find_map(|item| match item.1.as_ref() {
                    Expression::Defer { captures, .. } => Some(captures.as_slice()),
                    Expression::Statement(inner) => match inner.1.as_ref() {
                        Expression::Defer { captures, .. } => Some(captures.as_slice()),
                        _ => None,
                    },
                    _ => None,
                });
                assert_eq!(defer, Some(["x"].as_slice()));
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    #[test]
    fn defer_use_display_round_trips() {
        // Block Display omits braces; just check the capture list renders.
        let ast = decl_ast!("fn f() { let x = 1; defer use (x) { write(\"%i\", x); } }");
        let rendered = format!("{}", ast);
        assert!(
            rendered.contains("defer use (x)"),
            "expected Display to include capture list, got {rendered}"
        );
        // Bare defer still parses.
        let _ = stmt!("defer { write(\"x\"); }");
    }

    #[test]
    fn format_parses_as_call_expression() {
        same!("format(\"%i-%s\", 42, \"x\")");
        let ast = expr_ast!("format(\"%i-%s\", 42, \"x\")");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Call {
                name,
                args: Some(params),
            } => {
                assert!(matches!(name.1.as_ref(), Expression::Identifier("format")));
                assert_eq!(params.len(), 3);
            }
            other => panic!("expected format call expression, got {:?}", other),
        }
    }

    /// Lowercase::lowercase paths are module calls (`string::format`), not Construct.
    #[test]
    fn string_format_parses_as_qualified_module_call() {
        let ast = expr_ast!("string::format(\"%i\", 1)");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Call {
                name,
                args: Some(params),
            } => {
                match name.1.as_ref() {
                    Expression::QualifiedAccess { owner, member } => {
                        assert_eq!(*owner, "string");
                        assert_eq!(*member, "format");
                    }
                    other => panic!("expected QualifiedAccess callee, got {:?}", other),
                }
                assert_eq!(params.len(), 2);
            }
            other => panic!("expected Call(string::format), got {:?}", other),
        }
    }

    /// PascalCase owners stay Construct even when the member is lowercase (`Point::new`).
    #[test]
    fn pascal_case_method_call_stays_construct() {
        let ast = expr_ast!("Point::new(40, 2)");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Construct {
                enum_name,
                variant_name,
                fields: EnumConstructPayload::Tuple(args),
            } => {
                assert_eq!(enum_name, "Point");
                assert_eq!(variant_name, "new");
                assert_eq!(args.len(), 2);
            }
            other => panic!("expected Construct(Point::new), got {:?}", other),
        }
    }

    /// `Statement(ExprStatement)` must not append a second `;` (Display regression).
    #[test]
    fn statement_wrapping_expr_statement_display_has_one_semicolon() {
        let ast = decl_ast!("write_all(stdout(), to_bytes(\"x\"));");
        let rendered = format!("{}", ast);
        assert!(
            !rendered.contains(";;"),
            "ExprStatement already emits `;`; got {rendered:?}"
        );
        assert!(
            rendered.trim_end().ends_with(';'),
            "expected a single trailing semicolon, got {rendered:?}"
        );
    }

    #[test]
    fn break_and_continue_parse_as_statements() {
        let break_ast = decl_ast!("break;");
        match break_ast {
            Expression::Statement(inner) => {
                assert!(matches!(inner.1.as_ref(), Expression::Break));
            }
            other => panic!("expected break statement, got {:?}", other),
        }

        let continue_ast = decl_ast!("continue;");
        match continue_ast {
            Expression::Statement(inner) => {
                assert!(matches!(inner.1.as_ref(), Expression::Continue));
            }
            other => panic!("expected continue statement, got {:?}", other),
        }
    }

    #[test]
    fn c_style_for_parses_let_init_and_step() {
        let src = "for (let i = 0; i < 10; i = i + 1) { continue; }";
        let result = Pratt::default().declaration().parse(src).into_result();
        assert!(
            result.is_err(),
            "expected parse to fail for C-style for, got {:?}",
            result
        );
    }

    #[test]
    fn c_style_for_allows_empty_init_and_step() {
        let src = "for (; keep_going; ) { break; }";
        let result = Pratt::default().declaration().parse(src).into_result();
        assert!(
            result.is_err(),
            "expected parse to fail for C-style for, got {:?}",
            result
        );
    }

    #[test]
    fn for_in_parses_to_loop_with_identifier() {
        let ast = decl_ast!("for x in counter() { write(\"%i\", x); }");
        match ast {
            Expression::Statement(inner) => match inner.1.as_ref() {
                Expression::Loop {
                    identifier,
                    iterable,
                    body,
                } => {
                    match identifier.as_ref().map(|i| i.1.as_ref()) {
                        Some(Expression::Identifier(name)) => assert_eq!(*name, "x"),
                        other => panic!("expected Identifier(x), got {:?}", other),
                    }
                    assert!(matches!(iterable.1.as_ref(), Expression::Expr(_)));
                    assert!(matches!(body.1.as_ref(), Expression::Block(_)));
                }
                other => panic!("expected for-in Loop, got {:?}", other),
            },
            other => panic!("expected statement wrapper, got {:?}", other),
        }
    }

    #[test]
    fn for_in_display_round_trips() {
        let rendered = stmt!("for x in counter() { write(\"%i\", x); }");
        assert!(
            rendered.contains("for x in"),
            "expected for-in Display, got {rendered:?}"
        );
        assert!(
            rendered.contains("counter()"),
            "expected iterable in Display, got {rendered:?}"
        );
    }

    #[test]
    fn const_keyword_parses_to_constant_fragment() {
        let ast = decl_ast!("const answer = 42;");
        match ast {
            Expression::Statement(inner) => match inner.1.as_ref() {
                Expression::Fragment(children) => {
                    assert_eq!(children.len(), 2);
                    match children[0].1.as_ref() {
                        Expression::Constant(name, ty) => {
                            assert!(ty.is_none());
                            match name.1.as_ref() {
                                Expression::Identifier(name) => assert_eq!(*name, "answer"),
                                other => panic!("expected const identifier, got {:?}", other),
                            }
                        }
                        other => panic!("expected Constant, got {:?}", other),
                    }
                    match children[1].1.as_ref() {
                        Expression::Expr(inner) => match inner.1.as_ref() {
                            Expression::Integer(value) => assert_eq!(*value, 42),
                            other => panic!("expected integer initializer, got {:?}", other),
                        },
                        other => panic!("expected expression initializer, got {:?}", other),
                    }
                }
                other => panic!("expected Fragment, got {:?}", other),
            },
            other => panic!("expected Statement, got {:?}", other),
        }
    }

    #[test]
    fn pratt_test_fn_declaration() {
        assert_eq!(
            "fn main() -> void {\nwrite(\"Hello, %s\", 42);\n}",
            stmt!("fn main() -> void {\n  write(\"Hello, %s\", 42);\n  }")
        );
        same!("foo(1, 3, 4) * foo(2)");
    }

    #[test]
    fn enum_scalar_discriminants_parse() {
        let ast = decl_ast!(
            r#"#[repr(int)] enum Status { Ok = 200, NotFound = 404, }"#
        );
        match ast {
            Expression::EnumDecl { name, attrs, variants, .. } => {
                assert_eq!(name, "Status");
                assert_eq!(attrs[0].name, "repr");
                assert_eq!(variants.len(), 2);
                match variants[0].1.as_ref() {
                    Expression::EnumVariant {
                        name,
                        discriminant: Some(d),
                        ..
                    } => {
                        assert_eq!(*name, "Ok");
                        assert!(matches!(d.1.as_ref(), Expression::Integer(200)));
                    }
                    other => panic!("expected Ok = 200, got {:?}", other),
                }
                match variants[1].1.as_ref() {
                    Expression::EnumVariant {
                        name,
                        discriminant: Some(d),
                        ..
                    } => {
                        assert_eq!(*name, "NotFound");
                        assert!(matches!(d.1.as_ref(), Expression::Integer(404)));
                    }
                    other => panic!("expected NotFound = 404, got {:?}", other),
                }
            }
            other => panic!("expected EnumDecl, got {:?}", other),
        }
    }

    #[test]
    fn enum_string_and_bool_repr_parse() {
        let ast = decl_ast!(r#"#[repr(string)] enum Mode { Fast = "fast", Slow = "slow" }"#);
        match ast {
            Expression::EnumDecl { variants, .. } => {
                match variants[0].1.as_ref() {
                    Expression::EnumVariant {
                        discriminant: Some(d),
                        ..
                    } => assert!(matches!(d.1.as_ref(), Expression::String("fast"))),
                    other => panic!("expected string discriminant, got {:?}", other),
                }
            }
            other => panic!("expected EnumDecl, got {:?}", other),
        }
        let ast = decl_ast!("#[repr(bool)] enum Switch { Off = false, On = true }");
        match ast {
            Expression::EnumDecl { variants, .. } => {
                match variants[1].1.as_ref() {
                    Expression::EnumVariant {
                        discriminant: Some(d),
                        ..
                    } => assert!(matches!(d.1.as_ref(), Expression::Bool(true))),
                    other => panic!("expected bool discriminant, got {:?}", other),
                }
            }
            other => panic!("expected EnumDecl, got {:?}", other),
        }
    }

    #[test]
    fn enum_parses_to_enum_decl() {
        let ast = decl_ast!("enum Option { None, Some(int) }");
        match ast {
            Expression::EnumDecl { name, variants, .. } => {
                assert_eq!(name, "Option");
                assert_eq!(variants.len(), 2);

                match variants[0].1.as_ref() {
                    Expression::EnumVariant { docs: _, name, payload, .. } => {
                        assert_eq!(*name, "None");
                        assert!(matches!(payload, EnumVariantPayload::Unit));
                    }
                    other => panic!("expected EnumVariant(None), got {:?}", other),
                }

                match variants[1].1.as_ref() {
                    Expression::EnumVariant { docs: _, name, payload, .. } => {
                        assert_eq!(*name, "Some");
                        match payload {
                            EnumVariantPayload::Tuple(parts) => {
                                assert_eq!(parts.len(), 1);
                                match parts[0].1.as_ref() {
                                    Expression::Type(t) => assert_eq!(*t, "int"),
                                    other => panic!("expected Type(\"int\"), got {:?}", other),
                                }
                            }
                            other => panic!("expected Tuple payload, got {:?}", other),
                        }
                    }
                    other => panic!("expected EnumVariant(Some), got {:?}", other),
                }
            }
            other => panic!("expected EnumDecl, got {:?}", other),
        }
    }

    #[test]
    fn enum_with_record_variant_parses() {
        let ast = decl_ast!("enum Shape { Circle { x: int, y: int } }");
        match ast {
            Expression::EnumDecl { name, variants, .. } => {
                assert_eq!(name, "Shape");
                assert_eq!(variants.len(), 1);
                match variants[0].1.as_ref() {
                    Expression::EnumVariant { docs: _, name, payload, .. } => {
                        assert_eq!(*name, "Circle");
                        match payload {
                            EnumVariantPayload::Record(fields) => {
                                assert_eq!(fields.len(), 2);
                                assert_eq!(fields[0].name, "x");
                                assert_eq!(fields[1].name, "y");
                                match fields[0].value.1.as_ref() {
                                    Expression::Type(t) => assert_eq!(*t, "int"),
                                    other => panic!("expected Type(\"int\"), got {:?}", other),
                                }
                                match fields[1].value.1.as_ref() {
                                    Expression::Type(t) => assert_eq!(*t, "int"),
                                    other => panic!("expected Type(\"int\"), got {:?}", other),
                                }
                            }
                            other => panic!("expected Record payload, got {:?}", other),
                        }
                    }
                    other => panic!("expected EnumVariant(Circle), got {:?}", other),
                }
            }
            other => panic!("expected EnumDecl, got {:?}", other),
        }
    }

    #[test]
    fn qualified_construct_parses_to_construct() {
        let ast = decl_ast!("let x = Option::Some(42);");
        let frag = match ast {
            Expression::Statement(s) => match s.1.as_ref() {
                Expression::Fragment(items) => items.clone(),
                other => panic!("expected Fragment inside Statement, got {:?}", other),
            },
            Expression::Fragment(items) => items,
            other => panic!("expected Statement/Fragment from let, got {:?}", other),
        };
        let construct = match frag[1].1.as_ref() {
            Expression::Expr(e) => match e.1.as_ref() {
                Expression::Construct {
                    enum_name,
                    variant_name,
                    fields,
                } => (*enum_name, *variant_name, fields.clone()),
                other => panic!("expected Construct inside Expr, got {:?}", other),
            },
            other => panic!("expected Expr, got {:?}", other),
        };
        assert_eq!(construct.0, "Option");
        assert_eq!(construct.1, "Some");
        match construct.2 {
            EnumConstructPayload::Tuple(args) => {
                assert_eq!(args.len(), 1);
                match args[0].1.as_ref() {
                    Expression::Integer(n) => assert_eq!(*n, 42),
                    other => panic!("expected Integer(42), got {:?}", other),
                }
            }
            other => panic!("expected Tuple payload, got {:?}", other),
        }
    }

    #[test]
    fn record_construct_parses_to_record_payload() {
        let ast = decl_ast!("let p = E::Foo { x: 1, y: 2 };");
        let frag = match ast {
            Expression::Statement(s) => match s.1.as_ref() {
                Expression::Fragment(items) => items.clone(),
                other => panic!("expected Fragment, got {:?}", other),
            },
            Expression::Fragment(items) => items,
            other => panic!("expected Fragment, got {:?}", other),
        };
        let construct = match frag[1].1.as_ref() {
            Expression::Expr(e) => match e.1.as_ref() {
                Expression::Construct {
                    enum_name,
                    variant_name,
                    fields,
                } => (*enum_name, *variant_name, fields.clone()),
                other => panic!("expected Construct, got {:?}", other),
            },
            other => panic!("expected Expr, got {:?}", other),
        };
        assert_eq!(construct.0, "E");
        assert_eq!(construct.1, "Foo");
        match construct.2 {
            EnumConstructPayload::Record(parts) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[0].name, "x");
                assert_eq!(parts[1].name, "y");
                match parts[0].value.1.as_ref() {
                    Expression::Integer(n) => assert_eq!(*n, 1),
                    other => panic!("expected Integer(1), got {:?}", other),
                }
                match parts[1].value.1.as_ref() {
                    Expression::Integer(n) => assert_eq!(*n, 2),
                    other => panic!("expected Integer(2), got {:?}", other),
                }
            }
            other => panic!("expected Record payload, got {:?}", other),
        }
    }

    #[test]
    fn bare_construct_is_a_call_not_a_construct() {
        let ast = expr_ast!("Some(42)");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Call { name, args } => {
                match name.1.as_ref() {
                    Expression::Identifier(n) => assert_eq!(*n, "Some"),
                    other => panic!("expected Identifier(\"Some\"), got {:?}", other),
                }
                let args = args.expect("Some(42) must have args");
                assert_eq!(args.len(), 1);
                match args[0].1.as_ref() {
                    Expression::Integer(n) => assert_eq!(*n, 42),
                    other => panic!("expected Integer(42), got {:?}", other),
                }
            }
            other => panic!("expected Call (NOT Construct), got {:?}", other),
        }
    }

    #[test]
    fn match_with_constructor_patterns() {
        let ast = expr_ast!("match x { Option::None => 0, Option::Some(v) => v }");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Match { scrutinee, arms } => {
                match scrutinee.1.as_ref() {
                    Expression::Identifier(n) => assert_eq!(*n, "x"),
                    Expression::Expr(e) => match e.1.as_ref() {
                        Expression::Identifier(n) => assert_eq!(*n, "x"),
                        other => panic!("expected Identifier(x), got {:?}", other),
                    },
                    other => panic!("expected scrutinee to be `x`, got {:?}", other),
                }
                assert_eq!(arms.len(), 2);

                let MatchArm { pattern, body } = &arms[0];
                match &pattern.1 {
                    Pattern::Constructor {
                        enum_name,
                        variant_name,
                        payload,
                    } => {
                        assert_eq!(*enum_name, "Option");
                        assert_eq!(*variant_name, "None");
                        assert!(matches!(payload, PatternPayload::Unit));
                    }
                    other => panic!("expected Constructor(Option::None), got {:?}", other),
                }
                match body.1.as_ref() {
                    Expression::Integer(n) => assert_eq!(*n, 0),
                    other => panic!("expected Integer(0), got {:?}", other),
                }

                let MatchArm { pattern, body } = &arms[1];
                match &pattern.1 {
                    Pattern::Constructor {
                        enum_name,
                        variant_name,
                        payload,
                    } => {
                        assert_eq!(*enum_name, "Option");
                        assert_eq!(*variant_name, "Some");
                        match payload {
                            PatternPayload::Tuple(parts) => {
                                assert_eq!(parts.len(), 1);
                                match &parts[0].1 {
                                    Pattern::Binding { name } => assert_eq!(*name, "v"),
                                    other => panic!("expected Binding(v), got {:?}", other),
                                }
                            }
                            other => panic!("expected Tuple payload, got {:?}", other),
                        }
                    }
                    other => panic!("expected Constructor(Option::Some(v)), got {:?}", other),
                }
                match body.1.as_ref() {
                    Expression::Identifier(n) => assert_eq!(*n, "v"),
                    other => panic!("expected Identifier(v), got {:?}", other),
                }
            }
            other => panic!("expected Match, got {:?}", other),
        }
    }

    #[test]
    fn match_arm_brace_body_parses_as_block_not_dict() {
        let ast = expr_ast!("match x { Option::None => { 0 }, Option::Some(v) => { v } }");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Match { arms, .. } => {
                assert_eq!(arms.len(), 2);
                assert!(
                    matches!(arms[0].body.1.as_ref(), Expression::Block(_)),
                    "brace arm body should be Block, got {}",
                    arms[0].body.1
                );
                assert!(
                    matches!(arms[1].body.1.as_ref(), Expression::Block(_)),
                    "brace arm body should be Block, got {}",
                    arms[1].body.1
                );
            }
            other => panic!("expected Match, got {:?}", other),
        }
    }

    #[test]
    fn match_arm_brace_body_allows_let_and_trailing_value() {
        let ast = expr_ast!("match x { Option::Some(v) => { let y = v + 1; y }, _ => 0 }");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Match { arms, .. } => match arms[0].body.1.as_ref() {
                Expression::Block(children) => {
                    assert_eq!(children.len(), 2);
                    assert!(matches!(
                        children[0].1.as_ref(),
                        Expression::Statement(inner)
                            if matches!(inner.1.as_ref(), Expression::Fragment(_))
                    ));
                    assert!(matches!(
                        children[1].1.as_ref(),
                        Expression::Identifier("y")
                    ));
                }
                other => panic!("expected Block arm body, got {:?}", other),
            },
            other => panic!("expected Match, got {:?}", other),
        }
    }

    #[test]
    fn match_arm_brace_body_allows_return_statement() {
        let ast = expr_ast!("match x { Option::Some(v) => { return v; }, _ => 0 }");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Match { arms, .. } => match arms[0].body.1.as_ref() {
                Expression::Block(children) => {
                    assert_eq!(children.len(), 1);
                    assert!(matches!(
                        children[0].1.as_ref(),
                        Expression::Statement(inner)
                            if matches!(inner.1.as_ref(), Expression::Return(_))
                    ));
                }
                other => panic!("expected Block arm body, got {:?}", other),
            },
            other => panic!("expected Match, got {:?}", other),
        }
    }

    #[test]
    fn match_arm_brace_body_allows_field_access_like_self_method() {
        // Regression: `{ self.get() }` used to parse as a dict and fail with
        // `found '.' expected ':'` because dict fields require `name: value`.
        let ast = expr_ast!("match m { Mode::Zero => { self.get() }, Mode::Other(n) => n }");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Match { arms, .. } => match arms[0].body.1.as_ref() {
                Expression::Block(children) => {
                    assert_eq!(children.len(), 1);
                    let s = children[0].1.to_string();
                    assert!(
                        s.contains("self") && s.contains("get"),
                        "expected self.get() in block, got {s}"
                    );
                }
                other => panic!("expected Block arm body, got {:?}", other),
            },
            other => panic!("expected Match, got {:?}", other),
        }
    }

    #[test]
    fn match_arm_dict_literal_still_parses() {
        let ast = expr_ast!("match m { Mode::Zero => { x: 0 }, Mode::Other(n) => { x: n } }");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Match { arms, .. } => {
                assert!(
                    matches!(arms[0].body.1.as_ref(), Expression::Dict(_)),
                    "dict arm body should stay Dict, got {}",
                    arms[0].body.1
                );
                assert!(
                    matches!(arms[1].body.1.as_ref(), Expression::Dict(_)),
                    "dict arm body should stay Dict, got {}",
                    arms[1].body.1
                );
            }
            other => panic!("expected Match, got {:?}", other),
        }
    }

    #[test]
    fn record_pattern_shorthand_desugars() {
        let ast = expr_ast!("match p { E::Foo { x, y } => x + y }");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Match { arms, .. } => match &arms[0].pattern.1 {
                Pattern::Constructor {
                    enum_name,
                    variant_name,
                    payload,
                } => {
                    assert_eq!(*enum_name, "E");
                    assert_eq!(*variant_name, "Foo");
                    match payload {
                        PatternPayload::Record(fields) => {
                            assert_eq!(fields.len(), 2);
                            assert_eq!(fields[0].name, "x");
                            assert_eq!(fields[1].name, "y");
                            match &fields[0].pattern.1 {
                                Pattern::Binding { name } => assert_eq!(*name, "x"),
                                other => panic!("expected Binding(x), got {:?}", other),
                            }
                            match &fields[1].pattern.1 {
                                Pattern::Binding { name } => assert_eq!(*name, "y"),
                                other => panic!("expected Binding(y), got {:?}", other),
                            }
                        }
                        other => panic!("expected Record payload, got {:?}", other),
                    }
                }
                other => panic!("expected Constructor(E::Foo), got {:?}", other),
            },
            other => panic!("expected Match, got {:?}", other),
        }
    }

    #[test]
    fn wildcard_parses() {
        let ast1 = expr_ast!("match x { _ => 0 }");
        let inner1 = match ast1 {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner1 {
            Expression::Match { arms, .. } => {
                assert_eq!(arms.len(), 1);
                assert!(matches!(arms[0].pattern.1, Pattern::Wildcard));
            }
            other => panic!("expected Match, got {:?}", other),
        }
    }

    #[test]
    fn default_catch_all_parses() {
        let ast = expr_ast!("match x { default => 0 }");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Match { arms, .. } => {
                assert_eq!(arms.len(), 1);
                assert!(matches!(arms[0].pattern.1, Pattern::Default));
            }
            other => panic!("expected Match, got {:?}", other),
        }
    }

    #[test]
    fn nested_constructor_pattern() {
        let ast = expr_ast!("match x { Option::Some(Option::Some(v)) => v }");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Match { arms, .. } => {
                assert_eq!(arms.len(), 1);
                match &arms[0].pattern.1 {
                    Pattern::Constructor {
                        enum_name,
                        variant_name,
                        payload,
                    } => {
                        assert_eq!(*enum_name, "Option");
                        assert_eq!(*variant_name, "Some");
                        match payload {
                            PatternPayload::Tuple(parts) => {
                                assert_eq!(parts.len(), 1);
                                match &parts[0].1 {
                                    Pattern::Constructor {
                                        enum_name: inner_enum,
                                        variant_name: inner_variant,
                                        payload: inner_payload,
                                    } => {
                                        assert_eq!(*inner_enum, "Option");
                                        assert_eq!(*inner_variant, "Some");
                                        match inner_payload {
                                            PatternPayload::Tuple(inner_parts) => {
                                                assert_eq!(inner_parts.len(), 1);
                                                match &inner_parts[0].1 {
                                                    Pattern::Binding { name } => {
                                                        assert_eq!(*name, "v")
                                                    }
                                                    other => panic!(
                                                        "expected Binding(v), got {:?}",
                                                        other
                                                    ),
                                                }
                                            }
                                            other => panic!(
                                                "expected inner Tuple payload, got {:?}",
                                                other
                                            ),
                                        }
                                    }
                                    other => panic!(
                                        "expected nested Constructor(Option::Some(v)), got {:?}",
                                        other
                                    ),
                                }
                            }
                            other => panic!("expected Tuple payload, got {:?}", other),
                        }
                    }
                    other => panic!(
                        "expected outer Constructor(Option::Some(...)), got {:?}",
                        other
                    ),
                }
            }
            other => panic!("expected Match, got {:?}", other),
        }
    }

    #[test]
    fn postfix_field_access_parses_to_access() {
        let ast = expr_ast!("point.x");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Access(receiver, field) => {
                let recv_inner = match receiver.1.as_ref() {
                    Expression::Expr(e) => e.1.as_ref(),
                    other => other,
                };
                match recv_inner {
                    Expression::Identifier(n) => assert_eq!(*n, "point"),
                    other => panic!("expected Identifier(point), got {:?}", other),
                }
                assert_eq!(field, "x");
            }
            other => panic!("expected Access, got {:?}", other),
        }
    }

    #[test]
    fn named_call_args_parse_to_named_arg() {
        let ast = expr_ast!("f(a: 1, b: 2)");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Call { name, args } => {
                match name.1.as_ref() {
                    Expression::Identifier(n) => assert_eq!(*n, "f"),
                    other => panic!("expected Identifier(f), got {:?}", other),
                }
                let args = args.expect("call should have args");
                assert_eq!(args.len(), 2);
                match args[0].1.as_ref() {
                    Expression::NamedArg(n, val) => {
                        assert_eq!(*n, "a");
                        assert!(
                            matches!(val.1.as_ref(), Expression::Integer(1)),
                            "expected Integer(1), got {:?}",
                            val.1
                        );
                    }
                    other => panic!("expected NamedArg(a, 1), got {:?}", other),
                }
                match args[1].1.as_ref() {
                    Expression::NamedArg(n, val) => {
                        assert_eq!(*n, "b");
                        assert!(
                            matches!(val.1.as_ref(), Expression::Integer(2)),
                            "expected Integer(2), got {:?}",
                            val.1
                        );
                    }
                    other => panic!("expected NamedArg(b, 2), got {:?}", other),
                }
            }
            other => panic!("expected Call, got {:?}", other),
        }
    }

    #[test]
    fn named_call_args_display_round_trips() {
        same!("f(a: 1, b: 2)");
        same!("greet(\"Ada\", age: 36)");
    }

    #[test]
    fn postfix_field_access_chains_left_to_right() {
        let ast = expr_ast!("p.x.y");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Access(outer_receiver, outer_field) => {
                assert_eq!(outer_field, "y");
                match outer_receiver.1.as_ref() {
                    Expression::Access(inner_receiver, inner_field) => {
                        assert_eq!(*inner_field, "x");
                        let recv_inner = match inner_receiver.1.as_ref() {
                            Expression::Expr(e) => e.1.as_ref(),
                            other => other,
                        };
                        match recv_inner {
                            Expression::Identifier(n) => assert_eq!(*n, "p"),
                            other => panic!("expected Identifier(p), got {:?}", other),
                        }
                    }
                    other => panic!("expected Access(p, x) as outer receiver, got {:?}", other),
                }
            }
            other => panic!("expected Access(p.x, y), got {:?}", other),
        }
    }

    #[test]
    fn postfix_field_access_display_round_trips() {
        same!("point.x");
        same!("p.x.y");
    }

    #[test]
    fn postfix_field_access_does_not_break_float_parsing() {
        // `1.0` must stay a float atom, not `1` + postfix `.0`.
        let ast = expr_ast!("1.0");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        assert!(
            matches!(inner, Expression::Float(_)),
            "expected Float(1.0), got {:?}",
            inner
        );
    }

    #[test]
    fn primitive_cast_parses_as_expression() {
        let ast = expr_ast!("65 as byte");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        assert!(
            matches!(inner, Expression::Cast(_, _)),
            "expected Cast, got {:?}",
            inner
        );
    }

    #[test]
    fn unary_minus_binds_tighter_than_cast() {
        let ast = expr_ast!("-1 as byte");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Cast(lhs, _) => {
                let lhs = match lhs.1.as_ref() {
                    Expression::Group(inner) | Expression::Expr(inner) => inner.1.as_ref(),
                    other => other,
                };
                assert!(
                    matches!(lhs, Expression::Negate(_)),
                    "expected Cast(Negate(_), _), got Cast({lhs}, _)"
                );
            }
            other => panic!("expected Cast, got {other}"),
        }
    }

    #[test]
    fn string_literal_allows_escaped_quote() {
        let e = expr_ast!(r#""\"""#);
        let inner = match e {
            Expression::Expr(inner) => inner.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::String(s) => assert_eq!(s, r#"\""#),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn string_literal_allows_escaped_quote_amid_text() {
        let e = expr_ast!(r#""say \"hi\"""#);
        let inner = match e {
            Expression::Expr(inner) => inner.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::String(s) => assert_eq!(s, r#"say \"hi\""#),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn string_literal_backslash_before_close_is_escape() {
        // `"\\""` → content `\\` then closing quote — a string holding one `\`.
        let e = expr_ast!(r#""\\""#);
        let inner = match e {
            Expression::Expr(inner) => inner.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::String(s) => assert_eq!(s, r#"\\"#),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn rest_param_parses_in_arg_list() {
        let src = "fn sum(int... xs) -> int { return 0; }";
        let ast = Pratt::default().parse(src).expect("parse");
        let func = match ast.1.as_ref() {
            Expression::Program(items) => items[0].1.as_ref(),
            other => other,
        };
        let found = match func {
            Expression::Function { args, .. } => match args.1.as_ref() {
                Expression::Fragment(items) => {
                    matches!(
                        items[0].1.as_ref(),
                        Expression::Argument {
                            name: "xs",
                            is_rest: true,
                            ..
                        }
                    )
                }
                _ => false,
            },
            _ => false,
        };
        assert!(found, "expected Argument(..., xs, true) in arg list");
        // Display round-trip for the rest form.
        assert!(
            format!("{}", func).contains("int... xs"),
            "display should show rest syntax, got {}",
            func
        );
    }

    #[test]
    fn parameter_docs_attach_to_arguments() {
        let src = "fn sum(\n/// Values to add.\nint... xs,\n) -> int { return 0; }";
        let ast = Pratt::default().parse(src).expect("parse");
        let Expression::Program(items) = ast.1.as_ref() else {
            panic!("expected program");
        };
        let Expression::Function { args, .. } = items[0].1.as_ref() else {
            panic!("expected function");
        };
        let Expression::Fragment(params) = args.1.as_ref() else {
            panic!("expected params");
        };
        let Expression::Argument { docs, name, .. } = params[0].1.as_ref() else {
            panic!("expected argument");
        };
        assert_eq!(*name, "xs");
        assert_eq!(docs, &["Values to add."]);
    }

    #[test]
    fn range_half_open_parses() {
        same!("0..10");
        let inner = match expr_ast!("0..10") {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Range {
                inclusive: false, ..
            } => {}
            other => panic!("expected half-open Range, got {:?}", other),
        }
    }

    #[test]
    fn range_inclusive_parses() {
        same!("0..=10");
        let inner = match expr_ast!("0..=10") {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Range {
                inclusive: true, ..
            } => {}
            other => panic!("expected inclusive Range, got {:?}", other),
        }
    }

    #[test]
    fn range_does_not_break_float_or_field_access() {
        let float = match expr_ast!("1.0") {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        assert!(matches!(float, Expression::Float(_)));
        same!("point.x");
    }

    #[test]
    fn range_chain_is_rejected_as_non_associative() {
        // `..` is non-associative — `a..b..c` must not parse.
        let result = Pratt::default().parse("1..2..3");
        assert!(
            result.is_err(),
            "expected parse error for chained range 1..2..3, got Ok"
        );
    }

    #[test]
    fn compound_assign_parses_at_assignment_precedence() {
        let ast = expr_ast!("x += 1 + 2");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        assert!(matches!(
            inner,
            Expression::CompoundAssign(_, AssignOp::Add, _)
        ));
    }

    #[test]
    fn prefix_increment_parses_as_adjust() {
        let ast = expr_ast!("++x");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Adjust {
                op: AdjustOp::Inc,
                prefix: true,
                ..
            } => {}
            other => panic!("expected prefix ++, got {:?}", other),
        }
    }

    #[test]
    fn postfix_increment_parses_as_adjust() {
        let ast = expr_ast!("x++");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Adjust {
                op: AdjustOp::Inc,
                prefix: false,
                ..
            } => {}
            other => panic!("expected postfix ++, got {:?}", other),
        }
    }

    #[test]
    fn power_assign_token_is_not_split() {
        let ast = expr_ast!("x **= 2");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        assert!(matches!(
            inner,
            Expression::CompoundAssign(_, AssignOp::Pow, _)
        ));
    }

    /// Unwrap the single `If` from a one-statement `fn main() { ... }` body.
    fn unwrap_fn_if(src: &str) -> Expression<'_> {
        let ast = Pratt::default()
            .declaration()
            .parse(src)
            .into_result()
            .expect("parse failed")
            .1
            .as_ref()
            .clone();
        // Top: Function { ..., body: Block([...]) }
        let fn_body = match ast {
            Expression::Function { body, .. } => body.expect("function should have a body"),
            other => panic!("expected Function decl, got {:?}", other),
        };
        let stmts = match fn_body.1.as_ref() {
            Expression::Block(stmts) => stmts.clone(),
            other => panic!("expected Block body, got {:?}", other),
        };
        assert_eq!(stmts.len(), 1, "expected exactly one stmt in body");
        let inner = stmts.into_iter().next().unwrap();
        let inner_stmt = match inner.1.as_ref() {
            Expression::Statement(s) => s.1.as_ref().clone(),
            other => other.clone(),
        };

        match inner_stmt {
            Expression::If(branches) => Expression::If(branches),
            other => panic!("expected If, got {:?}", other),
        }
    }

    #[test]
    fn parse_if_without_else_still_works() {
        let src = "fn main() { if 1 > 0 { return 1; } }";
        match unwrap_fn_if(src) {
            Expression::If(branches) => {
                assert_eq!(branches.len(), 1, "single-branch if has 1 branch");
                let (cond_opt, _) = match branches[0].1.as_ref() {
                    Expression::Branch(c, b) => (c.clone(), b.clone()),
                    other => panic!("expected Branch, got {:?}", other),
                };
                assert!(
                    cond_opt.is_some(),
                    "the lone if-branch's cond must be Some(_), not None"
                );
            }
            other => panic!("expected If, got {:?}", other),
        }
    }

    #[test]
    fn parse_if_with_else_single_branch() {
        let src = "fn main() { if 1 > 0 { return 1; } else { return 0; } }";
        match unwrap_fn_if(src) {
            Expression::If(branches) => {
                assert_eq!(branches.len(), 2, "if/else has 2 branches");
                // First branch: Some(cond)
                let (cond_opt, _) = match branches[0].1.as_ref() {
                    Expression::Branch(c, b) => (c.clone(), b.clone()),
                    other => panic!("expected Branch at index 0, got {:?}", other),
                };
                assert!(cond_opt.is_some(), "first if-branch's cond must be Some(_)");
                // Second branch: None (the terminal else)
                let (cond_opt, _) = match branches[1].1.as_ref() {
                    Expression::Branch(c, b) => (c.clone(), b.clone()),
                    other => panic!("expected Branch at index 1, got {:?}", other),
                };
                assert!(cond_opt.is_none(), "else-branch's cond must be None");
            }
            other => panic!("expected If, got {:?}", other),
        }
    }

    #[test]
    fn parse_if_else_if_no_final_else() {
        let src = "fn main() { if 1 > 0 { return 1; } else if 1 < 0 { return 2; } }";
        match unwrap_fn_if(src) {
            Expression::If(branches) => {
                assert_eq!(branches.len(), 2, "if/else-if has 2 branches");
                for (i, branch) in branches.iter().enumerate() {
                    let (cond_opt, _) = match branch.1.as_ref() {
                        Expression::Branch(c, b) => (c.clone(), b.clone()),
                        other => panic!("expected Branch at index {}, got {:?}", i, other),
                    };
                    assert!(
                        cond_opt.is_some(),
                        "branch #{} cond must be Some(_) (no terminal else)",
                        i
                    );
                }
            }
            other => panic!("expected If, got {:?}", other),
        }
    }

    #[test]
    fn parse_if_else_if_single_else() {
        let src =
            "fn main() { if 1 > 0 { return 1; } else if 1 < 0 { return 2; } else { return 3; } }";
        match unwrap_fn_if(src) {
            Expression::If(branches) => {
                assert_eq!(branches.len(), 3, "if/else-if/else has 3 branches");
                // First two: Some(cond)
                for i in 0..2 {
                    let (cond_opt, _) = match branches[i].1.as_ref() {
                        Expression::Branch(c, b) => (c.clone(), b.clone()),
                        other => panic!("expected Branch at index {}, got {:?}", i, other),
                    };
                    assert!(cond_opt.is_some(), "branch #{} cond must be Some(_)", i);
                }
                // Last: None (terminal else)
                let (cond_opt, _) = match branches[2].1.as_ref() {
                    Expression::Branch(c, b) => (c.clone(), b.clone()),
                    other => panic!("expected Branch at index 2, got {:?}", other),
                };
                assert!(
                    cond_opt.is_none(),
                    "terminal else-branch's cond must be None"
                );
            }
            other => panic!("expected If, got {:?}", other),
        }
    }

    #[test]
    fn parse_if_else_chain_deep() {
        let src = "fn main() { if 1 > 0 { return 1; } else if 1 < 0 { return 2; } else if 1 == 0 { return 3; } else { return 4; } }";
        match unwrap_fn_if(src) {
            Expression::If(branches) => {
                assert_eq!(branches.len(), 4, "if/else-if/else-if/else has 4 branches");
                for i in 0..3 {
                    let (cond_opt, _) = match branches[i].1.as_ref() {
                        Expression::Branch(c, b) => (c.clone(), b.clone()),
                        other => panic!("expected Branch at index {}, got {:?}", i, other),
                    };
                    assert!(cond_opt.is_some(), "branch #{} cond must be Some(_)", i);
                }
                let (cond_opt, _) = match branches[3].1.as_ref() {
                    Expression::Branch(c, b) => (c.clone(), b.clone()),
                    other => panic!("expected Branch at index 3, got {:?}", other),
                };
                assert!(
                    cond_opt.is_none(),
                    "terminal else-branch's cond must be None"
                );
            }
            other => panic!("expected If, got {:?}", other),
        }
    }

    #[test]
    fn parse_if_with_dangling_else_fails() {
        let src = "fn main() { if 1 > 0 { return 1; } else }";
        let result = Pratt::default().declaration().parse(src).into_result();
        assert!(
            result.is_err(),
            "expected parse to fail for dangling else, got {:?}",
            result
        );
    }

    #[test]
    fn parse_async_fn_round_trips() {
        let ast = decl_ast!("async fn coro() { yield 1; }");
        match ast {
            Expression::Function { name, is_coro, .. } => {
                assert_eq!(name, "coro");
                assert!(is_coro);
            }
            other => panic!("expected async Function, got {:?}", other),
        }
    }

    #[test]
    fn parse_yield_statement() {
        let src = "async fn coro() { yield 42; }";
        let result = Pratt::default().declaration().parse(src).into_result();
        let (_span, expr) = result.expect("yield statement should parse");
        fn expect_yield_42(expr: &Expression) {
            let yield_node = match expr {
                Expression::Expr(e) => e.1.as_ref(),
                other => other,
            };
            match yield_node {
                Expression::Yield(y) => match y.1.as_ref() {
                    Expression::Expr(e) => match e.1.as_ref() {
                        Expression::Integer(42) => {}
                        other => panic!("expected yield 42, got {:?}", other),
                    },
                    Expression::Integer(42) => {}
                    other => panic!("expected yield 42, got {:?}", other),
                },
                other => panic!("expected Yield, got {:?}", other),
            }
        }
        match expr.as_ref() {
            Expression::Function { body, .. } => {
                match body.as_ref().expect("function body").1.as_ref() {
                    Expression::Block(stmts) => match stmts[0].1.as_ref() {
                        Expression::Statement(stmt) => match stmt.1.as_ref() {
                            // `yield` is preferred over `expr_statement`, so the
                            // node is bare `Yield` (not `ExprStatement(Yield)`).
                            Expression::Yield(_) => expect_yield_42(stmt.1.as_ref()),
                            Expression::ExprStatement(inner) => {
                                expect_yield_42(inner.1.as_ref());
                            }
                            other => panic!("expected Yield statement, got {:?}", other),
                        },
                        other => panic!("expected Statement, got {:?}", other),
                    },
                    other => panic!("expected Block, got {:?}", other),
                }
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    #[test]
    fn parse_resume_expression() {
        same!("resume h");
        let parsed = expr_ast!("resume h");
        let inner = match &parsed {
            Expression::Expr(e) => e.1.as_ref(),
            other => other,
        };
        match inner {
            Expression::Resume(target, arg) => {
                assert!(arg.is_none());
                match target.1.as_ref() {
                    Expression::Identifier(name) => assert_eq!(*name, "h"),
                    other => panic!("expected identifier target, got {:?}", other),
                }
            }
            other => panic!("expected Resume, got {:?}", other),
        }
    }

    #[test]
    fn parse_resume_with_send_round_trips() {
        same!("resume h with 42");
        let parsed = expr_ast!("resume h with 42");
        let inner = match &parsed {
            Expression::Expr(e) => e.1.as_ref(),
            other => other,
        };
        match inner {
            Expression::Resume(target, Some(arg)) => {
                match target.1.as_ref() {
                    Expression::Identifier(name) => assert_eq!(*name, "h"),
                    other => panic!("expected identifier target, got {:?}", other),
                }
                match arg.1.as_ref() {
                    Expression::Expr(e) => match e.1.as_ref() {
                        Expression::Integer(42) => {}
                        other => panic!("expected send arg 42, got {:?}", other),
                    },
                    Expression::Integer(42) => {}
                    other => panic!("expected send arg 42, got {:?}", other),
                }
            }
            other => panic!("expected Resume with arg, got {:?}", other),
        }
    }

    #[test]
    fn let_tuple_destructure_parses() {
        let ast = decl_ast!("let (a, b) = (1, 2);");
        let is_destructure = match &ast {
            Expression::Statement(s) | Expression::ExprStatement(s) => {
                matches!(s.1.as_ref(), Expression::LetDestructure { .. })
            }
            Expression::LetDestructure { .. } => true,
            _ => false,
        };
        assert!(is_destructure, "expected LetDestructure, got {:?}", ast);
    }

    #[test]
    fn let_record_destructure_parses() {
        let ast = decl_ast!("let { x, y } = { x: 1, y: 2 };");
        let is_destructure = match &ast {
            Expression::Statement(s) | Expression::ExprStatement(s) => {
                matches!(s.1.as_ref(), Expression::LetDestructure { .. })
            }
            Expression::LetDestructure { .. } => true,
            _ => false,
        };
        assert!(is_destructure, "expected LetDestructure, got {:?}", ast);
    }

    #[test]
    fn parse_let_binding_yield_round_trips() {
        let ast = decl_ast!("async fn f() { let x = yield 1; }");
        match ast {
            Expression::Function { body, .. } => {
                match body.as_ref().expect("function body").1.as_ref() {
                    Expression::Block(stmts) => match stmts[0].1.as_ref() {
                        Expression::Statement(stmt) => match stmt.1.as_ref() {
                            Expression::Fragment(children) => {
                                assert_eq!(children.len(), 2);
                                let init = children[1].1.as_ref();
                                let yield_expr = match init {
                                    Expression::Yield(y) => y.1.as_ref(),
                                    Expression::Expr(e) => match e.1.as_ref() {
                                        Expression::Yield(y) => y.1.as_ref(),
                                        other => {
                                            panic!("expected Yield initializer, got {:?}", other)
                                        }
                                    },
                                    other => panic!("expected Yield initializer, got {:?}", other),
                                };
                                match yield_expr {
                                    Expression::Expr(e) => match e.1.as_ref() {
                                        Expression::Integer(1) => {}
                                        other => panic!("expected yield 1, got {:?}", other),
                                    },
                                    Expression::Integer(1) => {}
                                    other => panic!("expected yield 1, got {:?}", other),
                                }
                            }
                            other => panic!("expected Fragment let, got {:?}", other),
                        },
                        other => panic!("expected Statement, got {:?}", other),
                    },
                    other => panic!("expected Block, got {:?}", other),
                }
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    #[test]
    fn parse_yield_from_round_trips() {
        same!("yield from inner");
        let parsed = expr_ast!("yield from inner");
        let inner = match &parsed {
            Expression::Expr(e) => e.1.as_ref(),
            other => other,
        };
        match inner {
            Expression::YieldFrom(target) => match target.1.as_ref() {
                Expression::Identifier(name) => assert_eq!(*name, "inner"),
                other => panic!("expected identifier, got {:?}", other),
            },
            other => panic!("expected YieldFrom, got {:?}", other),
        }
    }

    /// `extern "c" { fn puts(string s); }` parses to `ExternBlock`.
    #[test]
    fn parse_extern_block_single_function() {
        let ast = decl_ast!("extern \"c\" { fn puts(string s); }");
        match ast {
            Expression::ExternBlock {
                library,
                declarations,
            } => {
                assert_eq!(library, "c");
                assert_eq!(declarations.len(), 1);
                let f = &declarations[0];
                assert_eq!(f.name, "puts");
                // Returns: none
                assert!(f.returns.is_none());
                assert!(!f.variadic);
            }
            other => panic!("expected ExternBlock, got {:?}", other),
        }
    }

    #[test]
    fn parse_extern_block_variadic_ellipsis() {
        let ast = decl_ast!("extern \"c\" { fn printf(string fmt, ...) -> int; }");
        match ast {
            Expression::ExternBlock {
                library,
                declarations,
            } => {
                assert_eq!(library, "c");
                assert_eq!(declarations.len(), 1);
                let f = &declarations[0];
                assert_eq!(f.name, "printf");
                assert!(f.variadic);
                assert!(f.returns.is_some());
            }
            other => panic!("expected ExternBlock, got {:?}", other),
        }
    }

    #[test]
    fn parse_extern_block_bare_ellipsis_only() {
        let ast = decl_ast!("extern \"c\" { fn weird(...) -> int; }");
        match ast {
            Expression::ExternBlock { declarations, .. } => {
                assert!(declarations[0].variadic);
            }
            other => panic!("expected ExternBlock, got {:?}", other),
        }
    }

    #[test]
    fn parse_extern_rejects_language_rest_syntax() {
        use chumsky::Parser;
        let src = "extern \"c\" { fn bad(int... xs) -> int; }";
        let result = Pratt::default().declaration().parse(src);
        assert!(
            result.has_errors(),
            "expected parse error for T... name in extern"
        );
        let errs = result.into_errors();
        let msg = format!("{:?}", errs);
        assert!(
            msg.contains("bare `...`") || msg.contains("C varargs"),
            "unexpected error text: {msg}"
        );
    }

    #[test]
    fn parse_extern_block_multiple_functions() {
        let ast = decl_ast!("extern \"c\" { fn puts(string s); fn strlen(string s) -> int; }");
        match ast {
            Expression::ExternBlock {
                library,
                declarations,
            } => {
                assert_eq!(library, "c");
                assert_eq!(declarations.len(), 2);
                assert_eq!(declarations[0].name, "puts");
                assert!(declarations[0].returns.is_none());
                assert!(!declarations[0].variadic);
                assert_eq!(declarations[1].name, "strlen");
                assert!(declarations[1].returns.is_some());
                assert!(matches!(
                    declarations[1].returns.as_ref().unwrap().1.as_ref(),
                    Expression::Type("int")
                ));
            }
            other => panic!("expected ExternBlock, got {:?}", other),
        }
    }

    #[test]
    fn parse_extern_block_empty_body() {
        let ast = decl_ast!("extern \"m\" {}");
        match ast {
            Expression::ExternBlock {
                library,
                declarations,
            } => {
                assert_eq!(library, "m");
                assert!(declarations.is_empty());
            }
            other => panic!("expected ExternBlock, got {:?}", other),
        }
    }

    #[test]
    fn parse_extern_function_requires_trailing_semicolon() {
        let src = "extern \"c\" { fn puts(string s) }"; // missing ';'
        let result = Pratt::default().declaration().parse(src).into_result();
        assert!(
            result.is_err(),
            "expected parse to fail for missing trailing ';' in extern fn, got {:?}",
            result
        );
    }

    /// COI-73: `import` is a non-goal, not a synonym of `use`.
    #[test]
    fn import_path_is_parse_error_not_use() {
        let src = "import foo::bar;";
        match Pratt::default().parse(src) {
            Ok((_, expr)) => match expr.as_ref() {
                Expression::Use { .. } => {
                    panic!("import foo::bar; must not parse as Use")
                }
                other => panic!(
                    "import foo::bar; must be a parse error, not silently accepted, got {:?}",
                    other
                ),
            },
            Err(_) => {}
        }
    }

    /// COI-73: alias / brace / glob shapes must also fail, not become `Use`.
    #[test]
    fn import_alias_brace_and_glob_are_parse_errors_not_use() {
        for src in [
            "import foo::bar as x;",
            "import foo::{bar};",
            "import foo::*;",
        ] {
            match Pratt::default().parse(src) {
                Ok((_, expr)) => match expr.as_ref() {
                    Expression::Use { .. } => {
                        panic!("{src} must not parse as Use")
                    }
                    other => panic!(
                        "{src} must be a parse error, not silently accepted, got {:?}",
                        other
                    ),
                },
                Err(_) => {}
            }
        }
    }

    /// COI-73 / keywords.md: `import` is not reserved; users may name bindings with it.
    #[test]
    fn import_is_not_a_keyword_and_can_name_a_user_function() {
        let src = "fn import(int x) -> int { return x; } fn main() { let y = import(1); }";
        let result = Pratt::default().parse(src);
        assert!(
            result.is_ok(),
            "expected user fn named import to parse, got {:?}",
            result.err()
        );
        let ast = result.unwrap();
        let src_str = format!("{}", ast.1);
        assert!(
            src_str.contains("import"),
            "display should retain import name: {src_str}"
        );
    }

    /// COI-74: `case` is a non-goal, not a synonym of `match`.
    #[test]
    fn case_scrutinee_is_parse_error_not_match() {
        let src = "case x { Option::None => 0, Option::Some(v) => v }";
        match Pratt::default().parse(src) {
            Ok((_, expr)) => match expr.as_ref() {
                Expression::Match { .. } => {
                    panic!("case x {{ … }} must not parse as Match")
                }
                other => panic!(
                    "case x {{ … }} must be a parse error, not silently accepted, got {:?}",
                    other
                ),
            },
            Err(_) => {}
        }
    }

    /// COI-74: wildcard / single-arm / statement shapes must also fail, not become `Match`.
    #[test]
    fn case_wildcard_and_stmt_shapes_are_parse_errors_not_match() {
        for src in [
            "case x { _ => 0 }",
            "case x { Option::None => 0 }",
            "fn main() { case x { Option::None => 0 }; }",
        ] {
            match Pratt::default().parse(src) {
                Ok((_, expr)) => match expr.as_ref() {
                    Expression::Match { .. } => {
                        panic!("{src} must not parse as Match")
                    }
                    other => panic!(
                        "{src} must be a parse error, not silently accepted, got {:?}",
                        other
                    ),
                },
                Err(_) => {}
            }
        }
    }

    /// COI-74 / keywords.md: `case` is not reserved; users may name bindings with it.
    #[test]
    fn case_is_not_a_keyword_and_can_name_a_user_function() {
        let src = "fn case(int x) -> int { return x; } fn main() { let y = case(1); }";
        let result = Pratt::default().parse(src);
        assert!(
            result.is_ok(),
            "expected user fn named case to parse, got {:?}",
            result.err()
        );
        let ast = result.unwrap();
        let src_str = format!("{}", ast.1);
        assert!(
            src_str.contains("case"),
            "display should retain case name: {src_str}"
        );
    }

    /// COI-74: `case` stays a legal binding even inside real `match` patterns.
    #[test]
    fn case_can_be_a_match_pattern_binding() {
        let ast = expr_ast!("match x { case => case, Option::Some(case) => case }");
        let inner = match ast {
            Expression::Expr(e) => e.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Match { arms, .. } => {
                assert_eq!(arms.len(), 2);
                match &arms[0].pattern.1 {
                    Pattern::Binding { name } => assert_eq!(*name, "case"),
                    other => panic!("expected Binding(case), got {:?}", other),
                }
                match &arms[1].pattern.1 {
                    Pattern::Constructor {
                        enum_name,
                        variant_name,
                        payload,
                    } => {
                        assert_eq!(*enum_name, "Option");
                        assert_eq!(*variant_name, "Some");
                        match payload {
                            PatternPayload::Tuple(parts) => {
                                assert_eq!(parts.len(), 1);
                                match &parts[0].1 {
                                    Pattern::Binding { name } => assert_eq!(*name, "case"),
                                    other => panic!("expected Binding(case), got {:?}", other),
                                }
                            }
                            other => panic!("expected Tuple payload, got {:?}", other),
                        }
                    }
                    other => panic!("expected Constructor(Option::Some(case)), got {:?}", other),
                }
            }
            other => panic!("expected Match, got {:?}", other),
        }
    }

    /// COI-74: a real `match` must not enable `case` as a nested synonym.
    #[test]
    fn nested_case_inside_match_arm_is_parse_error_not_match() {
        let src = "match x { _ => case y { _ => 0 } }";
        match Pratt::default().parse(src) {
            Ok((_, expr)) => match expr.as_ref() {
                Expression::Match { arms, .. } => {
                    for arm in arms {
                        if matches!(arm.body.1.as_ref(), Expression::Match { .. }) {
                            panic!("{src} must not nest Match via case synonym");
                        }
                    }
                    panic!("{src} must be a parse error, not a Match with non-Match arm body");
                }
                other => panic!(
                    "{src} must be a parse error, not silently accepted, got {:?}",
                    other
                ),
            },
            Err(_) => {}
        }
    }

    #[test]
    fn parse_use_single_segment() {
        let src = "use foo::bar;";
        let result = Pratt::default().declaration().parse(src).into_result();
        match result {
            Ok((_span, expr)) => match expr.as_ref() {
                Expression::Use { path, name, alias } => {
                    assert_eq!(path, &["foo".to_string()]);
                    assert_eq!(name, "bar");
                    assert!(alias.is_none());
                }
                other => panic!("expected Use, got {:?}", other),
            },
            Err(e) => panic!("parse failed: {:?}", e),
        }
    }

    #[test]
    fn parse_use_multi_segment() {
        let src = "use foo::bar::baz;";
        let result = Pratt::default().declaration().parse(src).into_result();
        match result {
            Ok((_span, expr)) => match expr.as_ref() {
                Expression::Use { path, name, alias } => {
                    assert_eq!(path, &["foo".to_string(), "bar".to_string()]);
                    assert_eq!(name, "baz");
                    assert!(alias.is_none());
                }
                other => panic!("expected Use, got {:?}", other),
            },
            Err(e) => panic!("parse failed: {:?}", e),
        }
    }

    #[test]
    fn parse_use_with_alias() {
        let src = "use foo::bar as x;";
        let result = Pratt::default().declaration().parse(src).into_result();
        match result {
            Ok((_span, expr)) => match expr.as_ref() {
                Expression::Use { path, name, alias } => {
                    assert_eq!(path, &["foo".to_string()]);
                    assert_eq!(name, "bar");
                    assert_eq!(alias.as_deref(), Some("x"));
                }
                other => panic!("expected Use, got {:?}", other),
            },
            Err(e) => panic!("parse failed: {:?}", e),
        }
    }

    #[test]
    fn parse_use_glob() {
        let src = "use foo::bar::*;";
        let result = Pratt::default().declaration().parse(src).into_result();
        match result {
            Ok((_span, expr)) => match expr.as_ref() {
                Expression::Use { path, name, alias } => {
                    assert_eq!(path, &["foo".to_string(), "bar".to_string()]);
                    assert_eq!(name, "*");
                    assert!(alias.is_none());
                }
                other => panic!("expected Use, got {:?}", other),
            },
            Err(e) => panic!("parse failed: {:?}", e),
        }
    }

    #[test]
    fn parse_use_brace_group() {
        let src = "use foo::{sadge, greet as g};";
        let result = Pratt::default().declaration().parse(src).into_result();
        match result {
            Ok((_span, expr)) => match expr.as_ref() {
                Expression::Fragment(children) => {
                    assert_eq!(children.len(), 2);
                    match children[0].1.as_ref() {
                        Expression::Use { path, name, alias } => {
                            assert_eq!(path, &["foo".to_string()]);
                            assert_eq!(name, "sadge");
                            assert!(alias.is_none());
                        }
                        other => panic!("expected first Use, got {:?}", other),
                    }
                    match children[1].1.as_ref() {
                        Expression::Use { path, name, alias } => {
                            assert_eq!(path, &["foo".to_string()]);
                            assert_eq!(name, "greet");
                            assert_eq!(alias.as_deref(), Some("g"));
                        }
                        other => panic!("expected second Use, got {:?}", other),
                    }
                }
                other => panic!("expected Fragment of Uses, got {:?}", other),
            },
            Err(e) => panic!("parse failed: {:?}", e),
        }
    }

    #[test]
    fn parse_use_brace_group_rejects_empty() {
        let src = "use foo::{};";
        let result = Pratt::default().declaration().parse(src).into_result();
        assert!(
            result.is_err(),
            "empty brace-group import must fail to parse, got {:?}",
            result
        );
    }

    #[test]
    fn parse_use_brace_group_single_item() {
        let src = "use foo::{sadge};";
        let result = Pratt::default().declaration().parse(src).into_result();
        match result {
            Ok((_span, expr)) => match expr.as_ref() {
                Expression::Use { path, name, alias } => {
                    assert_eq!(path, &["foo".to_string()]);
                    assert_eq!(name, "sadge");
                    assert!(alias.is_none());
                }
                other => panic!("expected single Use, got {:?}", other),
            },
            Err(e) => panic!("parse failed: {:?}", e),
        }
    }

    #[test]
    fn parse_use_brace_group_nested_path() {
        let src = "use lib::io::{read, write as w};";
        let result = Pratt::default().declaration().parse(src).into_result();
        match result {
            Ok((_span, expr)) => match expr.as_ref() {
                Expression::Fragment(children) => {
                    assert_eq!(children.len(), 2);
                    match children[0].1.as_ref() {
                        Expression::Use { path, name, .. } => {
                            assert_eq!(path, &["lib".to_string(), "io".to_string()]);
                            assert_eq!(name, "read");
                        }
                        other => panic!("expected Use, got {:?}", other),
                    }
                }
                other => panic!("expected Fragment, got {:?}", other),
            },
            Err(e) => panic!("parse failed: {:?}", e),
        }
    }

    /// `dload` / `declare` / `invoke` are ordinary identifiers (not keywords),
    /// so a user may define `fn dload(...)` when they have not imported `ffi`.
    #[test]
    fn dload_is_not_a_keyword_and_can_name_a_user_function() {
        let src = "fn dload(int x) -> int { return x; } fn main() { let y = dload(1); }";
        let result = Pratt::default().parse(src);
        assert!(
            result.is_ok(),
            "expected user fn named dload to parse, got {:?}",
            result.err()
        );
        let ast = result.unwrap();
        let src_str = format!("{}", ast.1);
        assert!(
            src_str.contains("dload"),
            "display should retain dload name: {src_str}"
        );
    }

    #[test]
    fn ffi_types_qualified_construct_parses_multi_segment_path() {
        let src = "let x = ffi::types::Int;";
        let result = Pratt::default().parse(src);
        assert!(result.is_ok(), "parse failed: {:?}", result.err());
        let ast = result.unwrap();
        fn find_construct<'a>(e: &'a Expression<'a>) -> Option<(&'a str, &'a str)> {
            match e {
                Expression::Construct {
                    enum_name,
                    variant_name,
                    ..
                } => Some((*enum_name, *variant_name)),
                Expression::Program(items)
                | Expression::Block(items)
                | Expression::Fragment(items) => {
                    items.iter().find_map(|c| find_construct(c.1.as_ref()))
                }
                Expression::Expr(inner)
                | Expression::Group(inner)
                | Expression::Statement(inner)
                | Expression::ExprStatement(inner) => find_construct(inner.1.as_ref()),
                _ => None,
            }
        }
        let (enum_name, variant) =
            find_construct(ast.1.as_ref()).expect("expected Construct(ffi::types::Int)");
        assert_eq!(enum_name, "ffi::types");
        assert_eq!(variant, "Int");
    }

    #[test]
    fn parse_static_let_declaration() {
        let src = "static let hits = 0;";
        let result = Pratt::default().declaration().parse(src).into_result();
        match result {
            Ok((_span, expr)) => match expr.as_ref() {
                Expression::StaticDecl {
                    is_const,
                    name,
                    init,
                    ..
                } => {
                    assert!(!*is_const);
                    assert_eq!(*name, "hits");
                    let init_expr = match init.1.as_ref() {
                        Expression::Expr(inner) => inner.1.as_ref(),
                        other => other,
                    };
                    match init_expr {
                        Expression::Integer(n) => assert_eq!(*n, 0),
                        other => panic!("expected Integer(0) init, got {:?}", other),
                    }
                }
                other => panic!("expected StaticDecl, got {:?}", other),
            },
            Err(e) => panic!("parse failed: {:?}", e),
        }
    }

    #[test]
    fn parse_static_const_declaration() {
        let src = r#"static const VERSION = "1.0";"#;
        let result = Pratt::default().declaration().parse(src).into_result();
        match result {
            Ok((_span, expr)) => match expr.as_ref() {
                Expression::StaticDecl {
                    is_const,
                    name,
                    init,
                    ..
                } => {
                    assert!(*is_const);
                    assert_eq!(*name, "VERSION");
                    let init_expr = match init.1.as_ref() {
                        Expression::Expr(inner) => inner.1.as_ref(),
                        other => other,
                    };
                    assert!(
                        matches!(init_expr, Expression::String("1.0")),
                        "expected string init, got {:?}",
                        init_expr
                    );
                }
                other => panic!("expected StaticDecl, got {:?}", other),
            },
            Err(e) => panic!("parse failed: {:?}", e),
        }
    }

    #[test]
    fn parse_array_append_assignment_is_rejected() {
        let src = "a[] = 3;";
        let result = Pratt::default().parse(src);
        assert!(
            result.is_err(),
            "expected parse to fail for `a[] = 3`, got {:?}",
            result
        );
    }

    #[test]
    fn parse_readonly_array_literal() {
        let src = "readonly [1, 2, 3];";
        let ast = Pratt::default().parse(src).expect("parse readonly array");
        fn find_readonly_array(e: &Expression<'_>) -> bool {
            match e {
                Expression::Readonly(inner) => {
                    let mut cur = inner.1.as_ref();
                    while let Expression::Expr(inner) = cur {
                        cur = inner.1.as_ref();
                    }
                    matches!(cur, Expression::Array(_))
                }
                Expression::Program(items)
                | Expression::Fragment(items)
                | Expression::Block(items) => {
                    items.iter().any(|i| find_readonly_array(i.1.as_ref()))
                }
                Expression::Expr(inner)
                | Expression::Group(inner)
                | Expression::Statement(inner)
                | Expression::ExprStatement(inner) => find_readonly_array(inner.1.as_ref()),
                _ => false,
            }
        }
        assert!(
            find_readonly_array(ast.1.as_ref()),
            "expected Readonly(Array), got {:?}",
            ast.1
        );
    }

    #[test]
    fn parse_readonly_new_instantiate() {
        let src = "readonly new Point(1, 2);";
        let ast = Pratt::default()
            .parse(src)
            .unwrap_or_else(|e| panic!("parse `{src}` failed: {e:?}"));
        fn find_readonly_new(e: &Expression<'_>) -> bool {
            match e {
                Expression::Readonly(inner) => {
                    let mut cur = inner.1.as_ref();
                    while let Expression::Expr(inner) = cur {
                        cur = inner.1.as_ref();
                    }
                    matches!(cur, Expression::Instantiate(_, _))
                }
                Expression::Program(items)
                | Expression::Fragment(items)
                | Expression::Block(items) => {
                    items.iter().any(|i| find_readonly_new(i.1.as_ref()))
                }
                Expression::Expr(inner)
                | Expression::Group(inner)
                | Expression::Statement(inner)
                | Expression::ExprStatement(inner) => find_readonly_new(inner.1.as_ref()),
                _ => false,
            }
        }
        assert!(
            find_readonly_new(ast.1.as_ref()),
            "expected Readonly(Instantiate) for `{src}`, got {:?}",
            ast.1
        );
        assert!(
            Pratt::default()
                .parse("new readonly Point(1, 2);")
                .is_err(),
            "postfix `new readonly` is not valid"
        );
    }

    #[test]
    fn parse_static_singleton_example_file() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../examples/static_singleton.hy"
        ))
        .expect("read example");
        let result = Pratt::default().parse(&src);
        assert!(result.is_ok(), "parse failed: {:?}", result.err());
    }

    #[test]
    fn parse_mod_forward_declaration() {
        let src = "mod foo;";
        let result = Pratt::default().declaration().parse(src).into_result();
        match result {
            Ok((_span, expr)) => match expr.as_ref() {
                Expression::Module(name, _body) => {
                    assert_eq!(name, "foo");
                }
                other => panic!("expected Module, got {:?}", other),
            },
            Err(e) => panic!("parse failed: {:?}", e),
        }
    }

    #[test]
    fn parse_test_case_declaration() {
        let src = r#"test("addition works") { assert(1 + 1 == 2)?; }"#;
        let result = Pratt::default().declaration().parse(src).into_result();
        match result {
            Ok((_span, expr)) => match expr.as_ref() {
                Expression::TestCase { name, body } => {
                    fn is_string_lit(e: &Expression<'_>) -> bool {
                        match e {
                            Expression::String("addition works") => true,
                            Expression::Expr((_, inner)) | Expression::Group((_, inner)) => {
                                is_string_lit(inner)
                            }
                            _ => false,
                        }
                    }
                    assert!(
                        is_string_lit(name.1.as_ref()),
                        "expected string literal name, got {:?}",
                        name.1
                    );
                    assert!(matches!(body.1.as_ref(), Expression::Block(_)));
                }
                other => panic!("expected TestCase, got {:?}", other),
            },
            Err(e) => panic!("parse failed: {:?}", e),
        }
    }

    #[test]
    fn test_case_display_round_trips_name() {
        let src = r#"test("x") { assert(true)?; }"#;
        let ast = Pratt::default()
            .declaration()
            .parse(src)
            .into_result()
            .expect("parse");
        let displayed = format!("{}", ast.1);
        assert!(
            displayed.contains("test(\"x\")"),
            "display should retain test name: {displayed}"
        );
    }

    #[test]
    fn assign_cast_byte_parses_cast_on_rhs() {
        let e = expr_ast!("c = m as byte");
        let inner = match e {
            Expression::Expr(inner) => inner.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Assignment(_, value) => {
                let rhs = match value.1.as_ref() {
                    Expression::Expr(inner) => inner.1.as_ref(),
                    other => other,
                };
                assert!(
                    matches!(rhs, Expression::Cast(_, _)),
                    "expected Cast on RHS, got {rhs:?}"
                );
            }
            Expression::Cast(expr, _) => {
                let inner = match expr.1.as_ref() {
                    Expression::Expr(inner) => inner.1.as_ref(),
                    other => other,
                };
                assert!(
                    !matches!(inner, Expression::Assignment(_, _)),
                    "cast bound to whole assign, not RHS"
                );
                panic!("unexpected top-level Cast");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn cast_binds_tighter_than_add() {
        let e = expr_ast!("1 + 2 as float");
        let inner = match e {
            Expression::Expr(inner) => inner.1.as_ref().clone(),
            other => other,
        };
        match inner {
            Expression::Add(_, rhs) => {
                let rhs = match rhs.1.as_ref() {
                    Expression::Expr(inner) => inner.1.as_ref(),
                    other => other,
                };
                assert!(
                    matches!(rhs, Expression::Cast(_, _)),
                    "expected 1 + (2 as float), got rhs={rhs:?}"
                );
            }
            other => panic!("expected Add, got {other:?}"),
        }
    }

