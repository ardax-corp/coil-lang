    use super::*;
    use ast::Expression;

    // ── helpers ────────────────────────────────────────────────────────────────

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

    // ── type_alias with type params ────────────────────────────────────────────

    /// `type Id<T> = T;` — Display round-trips correctly.
    #[test]
    fn type_alias_with_single_type_param_round_trips() {
        assert_eq!(stmt!("type Id<T> = T;"), "type Id<T> = T;");
    }

    /// `type Pair<A, B> = (A, B);` — two type params, no bounds.
    #[test]
    fn type_alias_with_two_type_params_round_trips() {
        assert_eq!(
            stmt!("type Pair<A, B> = (A, B);"),
            "type Pair<A, B> = (A, B);"
        );
    }

    /// `type Num<T: Add + Mul> = T;` — single param with two bounds.
    #[test]
    fn type_alias_with_bounded_type_param_round_trips() {
        assert_eq!(
            stmt!("type Bounded<T: Add + Mul> = T;"),
            "type Bounded<T: Add + Mul> = T;"
        );
    }

    /// AST: `type Id<T> = T;` has one type param named `T` with no bounds.
    #[test]
    fn type_alias_type_param_ast_structure() {
        match decl_ast!("type Id<T> = T;") {
            Expression::TypeAlias {
                docs: _,
                name, type_params, ..
            } => {
                assert_eq!(name, "Id");
                assert_eq!(type_params.len(), 1);
                assert_eq!(type_params[0].name, "T");
                assert!(type_params[0].bounds.is_empty());
            }
            other => panic!("expected TypeAlias, got {:?}", other),
        }
    }

    /// AST: `type Bounded<T: Num + Eq> = T;` — bounds are recorded.
    #[test]
    fn type_alias_bounded_param_ast_structure() {
        match decl_ast!("type Bounded<T: Num + Eq> = T;") {
            Expression::TypeAlias { type_params, .. } => {
                assert_eq!(type_params.len(), 1);
                assert_eq!(type_params[0].name, "T");
                assert_eq!(type_params[0].bounds, vec!["Num", "Eq"]);
            }
            other => panic!("expected TypeAlias, got {:?}", other),
        }
    }

    // ── fn with type params ────────────────────────────────────────────────────

    /// `fn id<T>(T x) -> T {}` — single unbounded type param.
    #[test]
    fn fn_with_single_type_param_parses() {
        match decl_ast!("fn id<T>(T x) -> T {}") {
            Expression::Function {
                docs: _,
                name, type_params, ..
            } => {
                assert_eq!(name, "id");
                assert_eq!(type_params.len(), 1);
                assert_eq!(type_params[0].name, "T");
                assert!(type_params[0].bounds.is_empty());
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    /// `fn add<T: Num>(T a, T b) -> T {}` — one bounded type param.
    #[test]
    fn fn_with_bounded_type_param_parses() {
        match decl_ast!("fn add<T: Num>(T a, T b) -> T {}") {
            Expression::Function {
                docs: _,
                name, type_params, ..
            } => {
                assert_eq!(name, "add");
                assert_eq!(type_params.len(), 1);
                assert_eq!(type_params[0].name, "T");
                assert_eq!(type_params[0].bounds, vec!["Num"]);
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    /// `fn zip<A, B>(A a, B b) -> (A, B) {}` — two unbounded type params.
    #[test]
    fn fn_with_two_type_params_parses() {
        match decl_ast!("fn zip<A, B>(A a, B b) -> (A, B) {}") {
            Expression::Function {
                docs: _,
                name, type_params, ..
            } => {
                assert_eq!(name, "zip");
                assert_eq!(type_params.len(), 2);
                assert_eq!(type_params[0].name, "A");
                assert_eq!(type_params[1].name, "B");
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    /// `fn cmp<T: Eq + Ord>(T a, T b) -> bool {}` — multiple bounds.
    #[test]
    fn fn_with_multiple_bounds_parses() {
        match decl_ast!("fn cmp<T: Eq + Ord>(T a, T b) -> bool {}") {
            Expression::Function { type_params, .. } => {
                assert_eq!(type_params.len(), 1);
                assert_eq!(type_params[0].name, "T");
                assert_eq!(type_params[0].bounds, vec!["Eq", "Ord"]);
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    // ── fn with no type params (regression: type_params is empty) ─────────────

    /// Plain `fn main() {}` still has an empty `type_params` list.
    #[test]
    fn fn_without_type_params_has_empty_list() {
        match decl_ast!("fn main() {}") {
            Expression::Function { type_params, .. } => {
                assert!(type_params.is_empty());
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    // ── where clause ──────────────────────────────────────────────────────────

    /// `fn f<A, B>(A x) -> B where Convert<A, B> {}` — multi-param where.
    #[test]
    fn fn_with_multiparam_where_clause_parses() {
        match decl_ast!("fn f<A, B>(A x) -> B where Convert<A, B> {}") {
            Expression::Function {
                docs: _,
                name,
                type_params,
                where_constraints,
                ..
            } => {
                assert_eq!(name, "f");
                assert_eq!(type_params.len(), 2);
                assert_eq!(where_constraints.len(), 1);
                assert_eq!(where_constraints[0].class, "Convert");
                assert_eq!(where_constraints[0].args.len(), 2);
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    /// Unary `where Num<T>` parses alongside binder bounds remaining empty.
    #[test]
    fn fn_with_unary_where_clause_parses() {
        match decl_ast!("fn g<T>(T x) -> T where Num<T> {}") {
            Expression::Function {
                docs: _,
                where_constraints, ..
            } => {
                assert_eq!(where_constraints.len(), 1);
                assert_eq!(where_constraints[0].class, "Num");
                assert_eq!(where_constraints[0].args.len(), 1);
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    /// Display round-trips a multi-param where clause.
    #[test]
    fn fn_with_where_clause_display_round_trips() {
        let s = stmt!("fn f<A, B>(A x) -> B where Convert<A, B> {}");
        assert!(
            s.contains("where Convert<A, B>"),
            "expected where clause in display, got: {s}"
        );
        assert!(s.starts_with("fn f<A, B>"), "got: {s}");
    }

    // ── enum with type params ──────────────────────────────────────────────────

    /// `enum Option<T> { None, Some(T) }` — one type param.
    #[test]
    fn enum_with_single_type_param_parses() {
        match decl_ast!("enum Option<T> { None, Some(T), }") {
            Expression::EnumDecl {
                docs: _,
                name, type_params, ..
            } => {
                assert_eq!(name, "Option");
                assert_eq!(type_params.len(), 1);
                assert_eq!(type_params[0].name, "T");
            }
            other => panic!("expected EnumDecl, got {:?}", other),
        }
    }

    /// `enum Result<T, E> { Ok(T), Err(E) }` — two type params.
    #[test]
    fn enum_with_two_type_params_parses() {
        match decl_ast!("enum Result<T, E> { Ok(T), Err(E), }") {
            Expression::EnumDecl {
                docs: _,
                name, type_params, ..
            } => {
                assert_eq!(name, "Result");
                assert_eq!(type_params.len(), 2);
                assert_eq!(type_params[0].name, "T");
                assert_eq!(type_params[1].name, "E");
            }
            other => panic!("expected EnumDecl, got {:?}", other),
        }
    }

    // ── class with type params ─────────────────────────────────────────────────

    /// `class Box<T> { value: T, }` — one type param.
    #[test]
    fn class_with_single_type_param_parses() {
        match decl_ast!("class Box<T> { value: T, }") {
            Expression::Class {
                docs: _,
                name, type_params, ..
            } => {
                assert_eq!(name, "Box");
                assert_eq!(type_params.len(), 1);
                assert_eq!(type_params[0].name, "T");
            }
            other => panic!("expected Class, got {:?}", other),
        }
    }

    /// `class Pair<A, B: Ord> { first: A, second: B, }` — two params, one bounded.
    #[test]
    fn class_with_bounded_type_params_parses() {
        match decl_ast!("class Pair<A, B: Ord> { first: A, second: B, }") {
            Expression::Class {
                docs: _,
                name, type_params, ..
            } => {
                assert_eq!(name, "Pair");
                assert_eq!(type_params.len(), 2);
                assert_eq!(type_params[0].name, "A");
                assert!(type_params[0].bounds.is_empty());
                assert_eq!(type_params[1].name, "B");
                assert_eq!(type_params[1].bounds, vec!["Ord"]);
            }
            other => panic!("expected Class, got {:?}", other),
        }
    }

    // ── inherent impl with type params ─────────────────────────────────────────

    /// `impl Cell<T> { fn get() -> T {} }` → `Implementation` with one type param.
    #[test]
    fn inherent_impl_with_type_param_parses() {
        match decl_ast!("impl Cell<T> { fn get() -> T {} }") {
            Expression::Implementation {
                owner, type_params, ..
            } => {
                assert_eq!(owner, "Cell");
                assert_eq!(type_params.len(), 1);
                assert_eq!(type_params[0].name, "T");
            }
            other => panic!("expected Implementation, got {:?}", other),
        }
    }

    /// `impl Foo<T: Num + Eq> { fn bar() {} }` — bounded param.
    #[test]
    fn inherent_impl_with_bounded_type_param_parses() {
        match decl_ast!("impl Foo<T: Num + Eq> { fn bar() {} }") {
            Expression::Implementation {
                owner, type_params, ..
            } => {
                assert_eq!(owner, "Foo");
                assert_eq!(type_params.len(), 1);
                assert_eq!(type_params[0].name, "T");
                assert_eq!(type_params[0].bounds, vec!["Num", "Eq"]);
            }
            other => panic!("expected Implementation, got {:?}", other),
        }
    }

    /// `impl Point { fn sum() {} }` — no type params, inherent impl.
    #[test]
    fn inherent_impl_without_type_params_parses() {
        match decl_ast!("impl Point { fn sum() {} }") {
            Expression::Implementation {
                owner, type_params, ..
            } => {
                assert_eq!(owner, "Point");
                assert!(type_params.is_empty());
            }
            other => panic!("expected Implementation, got {:?}", other),
        }
    }

    // ── typeclass impl (primitive type args) ───────────────────────────────────

    #[test]
    fn typeclass_impl_uses_for_form() {
        match decl_ast!("impl Num for int { fn add(int a, int b) -> int {} }") {
            Expression::TypeClassImpl { class, args, .. } => {
                assert_eq!(class, "Num");
                assert_eq!(args.len(), 1);
                assert!(matches!(args[0].1.as_ref(), Expression::Type("int")));
            }
            other => panic!("expected TypeClassImpl, got {:?}", other),
        }
    }

    #[test]
    fn typeclass_angle_form_is_rejected() {
        let result = Pratt::default()
            .declaration()
            .parse("impl Num<int> { fn add(int a, int b) -> int {} }")
            .into_result();
        assert!(
            result.is_err(),
            "expected `impl Trait<Type>` to fail; use `impl Trait for Type`"
        );
    }

    // ── typeclass decl ─────────────────────────────────────────────────────────

    /// `trait Eq<T> { fn eq(T a, T b) -> bool; }` — sig-only method.
    #[test]
    fn typeclass_with_sig_only_method_parses() {
        match decl_ast!("trait Eq<T> { fn eq(T a, T b) -> bool; }") {
            Expression::TypeClass {
                docs: _,
                name,
                type_params,
                methods,
            } => {
                assert_eq!(name, "Eq");
                assert_eq!(type_params.len(), 1);
                assert_eq!(type_params[0].name, "T");
                assert_eq!(methods.len(), 1);
                // The sig-only method is a Function with an empty Block body.
                match methods[0].1.as_ref() {
                    Expression::Function {
                        docs: _,
                        name: mname, body, ..
                    } => {
                        assert_eq!(*mname, "eq");
                        assert!(matches!(
                            body.as_ref().expect("method body").1.as_ref(),
                            Expression::Block(stmts) if stmts.is_empty()
                        ));
                    }
                    other => panic!("expected Function, got {:?}", other),
                }
            }
            other => panic!("expected TypeClass, got {:?}", other),
        }
    }

    /// `trait Num<T> { fn add(T a, T b) -> T { return a + b; } }` — default method.
    #[test]
    fn typeclass_with_default_method_parses() {
        match decl_ast!("trait Num<T> { fn add(T a, T b) -> T { return a + b; } }") {
            Expression::TypeClass {
                docs: _,
                name,
                type_params,
                methods,
            } => {
                assert_eq!(name, "Num");
                assert_eq!(type_params.len(), 1);
                assert_eq!(methods.len(), 1);
                // A default method has a non-empty block.
                match methods[0].1.as_ref() {
                    Expression::Function {
                        docs: _,
                        name: mname, body, ..
                    } => {
                        assert_eq!(*mname, "add");
                        // Block is non-empty (contains the return statement).
                        assert!(matches!(
                            body.as_ref().expect("method body").1.as_ref(),
                            Expression::Block(stmts) if !stmts.is_empty()
                        ));
                    }
                    other => panic!("expected Function, got {:?}", other),
                }
            }
            other => panic!("expected TypeClass, got {:?}", other),
        }
    }

    /// `trait Ord<T: Eq> { fn lt(T a, T b) -> bool; fn gt(T a, T b) -> bool; }` — two sig-only.
    #[test]
    fn typeclass_with_bounded_param_and_two_methods_parses() {
        match decl_ast!("trait Ord<T: Eq> { fn lt(T a, T b) -> bool; fn gt(T a, T b) -> bool; }") {
            Expression::TypeClass {
                docs: _,
                name,
                type_params,
                methods,
            } => {
                assert_eq!(name, "Ord");
                assert_eq!(type_params.len(), 1);
                assert_eq!(type_params[0].bounds, vec!["Eq"]);
                assert_eq!(methods.len(), 2);
            }
            other => panic!("expected TypeClass, got {:?}", other),
        }
    }

    /// `trait Show { fn show() -> string; }` — no type params (plain trait).
    #[test]
    fn typeclass_without_type_params_parses() {
        match decl_ast!("trait Show { fn show() -> string; }") {
            Expression::TypeClass {
                docs: _,
                name,
                type_params,
                methods,
            } => {
                assert_eq!(name, "Show");
                assert!(type_params.is_empty());
                assert_eq!(methods.len(), 1);
            }
            other => panic!("expected TypeClass, got {:?}", other),
        }
    }

    // ── forall type annotations ────────────────────────────────────────────────

    /// `type F = forall T. T;` — single unbounded forall param in type alias.
    #[test]
    fn forall_in_type_alias_parses() {
        match decl_ast!("type F = forall T. T;") {
            Expression::TypeAlias {
                docs: _,
                ty,
                type_params: alias_params,
                ..
            } => {
                assert!(alias_params.is_empty()); // the alias itself has no params
                match ty.1.as_ref() {
                    Expression::Forall { params, ty: inner } => {
                        assert_eq!(params.len(), 1);
                        assert_eq!(params[0].name, "T");
                        assert!(params[0].bounds.is_empty());
                        // inner type is `T` (an identifier / Type node)
                        assert!(matches!(
                            inner.1.as_ref(),
                            Expression::Type("T") | Expression::Identifier("T")
                        ));
                    }
                    other => panic!("expected Forall, got {:?}", other),
                }
            }
            other => panic!("expected TypeAlias, got {:?}", other),
        }
    }

    /// `type F = forall T: Num. T;` — single bounded forall param.
    #[test]
    fn forall_with_bounded_param_in_type_alias_parses() {
        match decl_ast!("type F = forall T: Num. T;") {
            Expression::TypeAlias { ty, .. } => match ty.1.as_ref() {
                Expression::Forall { params, .. } => {
                    assert_eq!(params.len(), 1);
                    assert_eq!(params[0].name, "T");
                    assert_eq!(params[0].bounds, vec!["Num"]);
                }
                other => panic!("expected Forall, got {:?}", other),
            },
            other => panic!("expected TypeAlias, got {:?}", other),
        }
    }

    /// `type F = forall T, U. T;` — two unbounded forall params.
    #[test]
    fn forall_with_two_params_in_type_alias_parses() {
        match decl_ast!("type F = forall T, U. T;") {
            Expression::TypeAlias { ty, .. } => match ty.1.as_ref() {
                Expression::Forall { params, .. } => {
                    assert_eq!(params.len(), 2);
                    assert_eq!(params[0].name, "T");
                    assert_eq!(params[1].name, "U");
                }
                other => panic!("expected Forall, got {:?}", other),
            },
            other => panic!("expected TypeAlias, got {:?}", other),
        }
    }

    /// `forall T. T` Display round-trip: `forall T. T`
    #[test]
    fn forall_display_round_trips() {
        assert_eq!(stmt!("type F = forall T. T;"), "type F = forall T. T;");
    }

    // ── Display round-trips for new forms ─────────────────────────────────────

    /// `type Id<T> = T;` (already tested above — extra sanity check).
    #[test]
    fn type_alias_with_param_display_is_stable() {
        // Parse and re-display must be identity.
        let s = stmt!("type Map<K, V> = (K, V);");
        assert_eq!(s, "type Map<K, V> = (K, V);");
    }

    /// TypeClass Display: `trait Eq<T> { … }`
    #[test]
    fn typeclass_display_round_trips() {
        // The Display impl omits the function bodies' args (unhandled Display)
        // so we only check that the outer structure round-trips and contains
        // the expected substrings.
        let s = stmt!("trait Show { fn show() -> string; }");
        assert!(s.starts_with("trait Show {"), "got: {s}");
        assert!(s.contains("show"), "got: {s}");
    }

    /// `F: * -> *` parses as an Arrow-kinded type parameter.
    #[test]
    fn constructor_kind_annotation_parses() {
        match decl_ast!("trait Container<F: * -> *> { fn first<A>(F<A> xs) -> A; }") {
            Expression::TypeClass { type_params, .. } => {
                assert_eq!(type_params.len(), 1);
                assert_eq!(type_params[0].name, "F");
                assert_eq!(
                    type_params[0].kind,
                    crate::ast::Kind::Arrow(
                        Box::new(crate::ast::Kind::Type),
                        Box::new(crate::ast::Kind::Type)
                    )
                );
                assert!(type_params[0].bounds.is_empty());
            }
            other => panic!("expected TypeClass, got {:?}", other),
        }
    }

    #[test]
    fn constraint_kind_annotation_parses() {
        match decl_ast!("fn apply_c<c: * -> Constraint, T: c>(T x) -> string { return show(x); }") {
            Expression::Function { type_params, .. } => {
                assert_eq!(type_params.len(), 2);
                assert_eq!(type_params[0].name, "c");
                assert_eq!(
                    type_params[0].kind,
                    crate::ast::Kind::Arrow(
                        Box::new(crate::ast::Kind::Type),
                        Box::new(crate::ast::Kind::Constraint)
                    )
                );
                assert_eq!(type_params[1].name, "T");
                assert_eq!(type_params[1].bounds, vec!["c"]);
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    #[test]
    fn binary_hkt_kind_annotation_is_right_associative() {
        match decl_ast!("trait Bifunctor<F: * -> * -> *> { fn tag<A, B>(F<A, B> xs) -> int; }") {
            Expression::TypeClass { type_params, .. } => {
                assert_eq!(
                    type_params[0].kind,
                    crate::ast::Kind::Arrow(
                        Box::new(crate::ast::Kind::Type),
                        Box::new(crate::ast::Kind::Arrow(
                            Box::new(crate::ast::Kind::Type),
                            Box::new(crate::ast::Kind::Type)
                        ))
                    )
                );
            }
            other => panic!("expected TypeClass, got {:?}", other),
        }
    }

    #[test]
    fn parenthesized_kind_annotation_parses() {
        match decl_ast!("trait Higher<F: (* -> *) -> *> { fn tag<G: * -> *>(F<G> xs) -> int; }") {
            Expression::TypeClass { type_params, .. } => {
                assert_eq!(
                    type_params[0].kind,
                    crate::ast::Kind::Arrow(
                        Box::new(crate::ast::Kind::Arrow(
                            Box::new(crate::ast::Kind::Type),
                            Box::new(crate::ast::Kind::Type)
                        )),
                        Box::new(crate::ast::Kind::Type)
                    )
                );
            }
            other => panic!("expected TypeClass, got {:?}", other),
        }
    }

    #[test]
    fn kind_annotation_can_be_followed_by_bound() {
        match decl_ast!(
            "fn use_bi<F: * -> * -> *, Bifunctor, A, B>(F<A, B> xs) -> int { return 0; }"
        ) {
            Expression::Function { type_params, .. } => {
                assert_eq!(type_params.len(), 3);
                assert_eq!(type_params[0].name, "F");
                assert_eq!(type_params[0].bounds, vec!["Bifunctor"]);
                assert_eq!(type_params[1].name, "A");
                assert_eq!(type_params[2].name, "B");
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    /// Display keeps constructor-kind annotations on type params.
    #[test]
    fn constructor_kind_display_round_trips() {
        let s = stmt!("trait Container<F: * -> *> { fn first<A>(F<A> xs) -> A; }");
        assert!(
            s.contains("F: * -> *"),
            "expected kind annotation in display, got: {s}"
        );
    }

    #[test]
    fn binary_hkt_kind_display_round_trips() {
        let s = stmt!("trait Bifunctor<F: * -> * -> *> { fn tag<A, B>(F<A, B> xs) -> int; }");
        assert!(
            s.contains("F: * -> * -> *"),
            "expected binary kind annotation in display, got: {s}"
        );
    }

    #[test]
    fn constraint_kind_display_round_trips() {
        let s = stmt!("fn apply_c<c: * -> Constraint, T: c>(T x) -> string { return show(x); }");
        assert!(
            s.contains("c: * -> Constraint"),
            "expected constraint kind annotation in display, got: {s}"
        );
        assert!(
            s.contains("T: c"),
            "expected abstract bound in display, got: {s}"
        );
    }

    /// TypeClassImpl Display: `impl Num for int { … }`
    #[test]
    fn typeclass_impl_display_round_trips() {
        let s = stmt!("impl Num for int {}");
        assert_eq!(s, "impl Num for int {  }");
    }

    /// Preferred form: `impl Show for Point`.
    #[test]
    fn trait_impl_for_parses_and_prepends_self() {
        match decl_ast!("impl Show for Point { fn show(Point p) -> string {} }") {
            Expression::TypeClassImpl { class, args, .. } => {
                assert_eq!(class, "Show");
                assert_eq!(args.len(), 1);
                assert!(matches!(*args[0].1, Expression::Type("Point")));
            }
            other => panic!("expected TypeClassImpl, got {other:?}"),
        }
    }

    /// `impl Thing<string, int> for Message` → args [Message, string, int].
    #[test]
    fn trait_impl_for_with_bracket_args_prepends_self() {
        match decl_ast!(
            "impl Thing<string, int> for Message { fn do_something(Message m, string x) -> int {} }"
        ) {
            Expression::TypeClassImpl { class, args, .. } => {
                assert_eq!(class, "Thing");
                assert_eq!(args.len(), 3);
                assert!(matches!(*args[0].1, Expression::Type("Message")));
                assert!(matches!(*args[1].1, Expression::Type("string")));
                assert!(matches!(*args[2].1, Expression::Type("int")));
            }
            other => panic!("expected TypeClassImpl, got {other:?}"),
        }
    }

    #[test]
    fn trait_impl_for_display_round_trips() {
        let s = stmt!("impl Show for Point {}");
        assert_eq!(s, "impl Show for Point {  }");
        let s2 = stmt!("impl Thing<string, int> for Message {}");
        assert_eq!(s2, "impl Thing<string, int> for Message {  }");
    }

    /// Phase 6: associated type decl + projection parse / Display round-trip.
    #[test]
    fn assoc_type_decl_and_projection_round_trip() {
        match decl_ast!("trait Collect<C> { type Elem; fn head(C xs) -> Elem; }") {
            Expression::TypeClass { methods, .. } => {
                assert!(
                    methods.iter().any(|m| matches!(
                        m.1.as_ref(),
                        Expression::AssocTypeDecl { name: "Elem", .. }
                    )),
                    "expected AssocTypeDecl Elem, got {:?}",
                    methods
                );
                // Return type of head should be bare Type("Elem") (resolved as assoc later).
                let head = methods.iter().find_map(|m| match m.1.as_ref() {
                    Expression::Function {
                        docs: _,
                        name: "head",
                        returns: Some(r),
                        ..
                    } => Some(r.1.as_ref()),
                    _ => None,
                });
                assert!(
                    matches!(head, Some(Expression::Type("Elem"))),
                    "expected bare Elem return, got {:?}",
                    head
                );
            }
            other => panic!("expected TypeClass, got {:?}", other),
        }

        let proj = Pratt::default()
            .type_annotation()
            .parse("Collect::Elem")
            .into_result()
            .expect("projection parse failed");
        assert!(
            matches!(
                proj.1.as_ref(),
                Expression::TypeProjection {
                    owner: "Collect",
                    name: "Elem",
                    args,
                }
                if args.is_empty()
            ),
            "expected TypeProjection, got {:?}",
            proj.1
        );
        assert_eq!(format!("{}", proj.1), "Collect::Elem");

        let s = stmt!("trait Collect<C> { type Elem; fn head(C xs) -> Elem; }");
        assert!(s.contains("type Elem;"), "got: {s}");
        assert!(s.contains("Collect"), "got: {s}");
    }

    /// Phase 6: assoc type def inside typeclass impl.
    #[test]
    fn assoc_type_def_in_impl_parses() {
        match decl_ast!(
            "impl Collect for Option<int> { type Elem = int; fn head(Option<int> xs) -> int { return 0; } }"
        ) {
            Expression::TypeClassImpl { methods, .. } => {
                assert!(
                    methods.iter().any(|m| matches!(
                        m.1.as_ref(),
                        Expression::AssocTypeDef { name: "Elem", .. }
                    )),
                    "expected AssocTypeDef Elem, got {:?}",
                    methods
                );
            }
            other => panic!("expected TypeClassImpl, got {:?}", other),
        }
    }

    #[test]
    fn generic_assoc_type_decl_parses() {
        match decl_ast!("trait Pointer<P> { type Ref<T>; fn get<T>(P p) -> P::Ref<T>; }") {
            Expression::TypeClass { methods, .. } => {
                let assoc = methods.iter().find_map(|m| match m.1.as_ref() {
                    Expression::AssocTypeDecl {
                        name: "Ref",
                        type_params,
                    } => Some(type_params),
                    _ => None,
                });
                let params = assoc.expect("expected Ref associated type declaration");
                assert_eq!(params.len(), 1);
                assert_eq!(params[0].name, "T");
            }
            other => panic!("expected TypeClass, got {:?}", other),
        }
    }

    #[test]
    fn generic_assoc_type_def_in_impl_parses() {
        match decl_ast!(
            "impl Pointer for Box { type Ref<T> = T; fn get<T>(Box p) -> T { return 0; } }"
        ) {
            Expression::TypeClassImpl { methods, .. } => {
                let assoc = methods.iter().find_map(|m| match m.1.as_ref() {
                    Expression::AssocTypeDef {
                        name: "Ref",
                        type_params,
                        ..
                    } => Some(type_params),
                    _ => None,
                });
                let params = assoc.expect("expected Ref associated type definition");
                assert_eq!(params.len(), 1);
                assert_eq!(params[0].name, "T");
            }
            other => panic!("expected TypeClassImpl, got {:?}", other),
        }
    }

    #[test]
    fn generic_assoc_type_projection_parses_and_displays() {
        let proj = Pratt::default()
            .type_annotation()
            .parse("Pointer::Ref<int>")
            .into_result()
            .expect("projection parse failed");
        match proj.1.as_ref() {
            Expression::TypeProjection { owner, name, args } => {
                assert_eq!((*owner, *name), ("Pointer", "Ref"));
                assert_eq!(args.len(), 1);
                assert!(matches!(args[0].1.as_ref(), Expression::Type("int")));
            }
            other => panic!("expected TypeProjection, got {:?}", other),
        }
        assert_eq!(format!("{}", proj.1), "Pointer::Ref<int>");
    }

    /// Inherent impl Display: `impl Point { … }`
    #[test]
    fn inherent_impl_display_round_trips() {
        let s = stmt!("impl Point {}");
        assert_eq!(s, "impl Point {  }");
    }

    /// Inherent impl with type param Display: `impl Cell<T> { … }`
    #[test]
    fn inherent_impl_with_type_param_display_round_trips() {
        let s = stmt!("impl Cell<T> {}");
        assert_eq!(s, "impl Cell<T> {  }");
    }

    /// `#[derive(Show, Eq)]` on enums parses attribute traits.
    #[test]
    fn enum_derive_attr_parses_traits() {
        match decl_ast!("#[derive(Show, Eq)] enum Point { Origin, Point { x: int, y: int } }") {
            Expression::EnumDecl { name, attrs, .. } => {
                assert_eq!(name, "Point");
                assert_eq!(attrs.len(), 1);
                assert_eq!(attrs[0].name, "derive");
                assert!(matches!(
                    &attrs[0].args,
                    ast::AttrArgs::Idents(v) if v == &["Show", "Eq"]
                ));
            }
            other => panic!("expected EnumDecl, got {:?}", other),
        }
    }

    /// Derive attribute Display round-trips on enums.
    #[test]
    fn enum_derive_attr_display_round_trips() {
        let s = stmt!("#[derive(Show, Eq)] enum Point { Origin }");
        assert_eq!(s, "#[derive(Show, Eq)]\nenum Point { Origin }");
    }

    /// `#[derive(Show, Eq)]` on classes parses attribute traits.
    #[test]
    fn class_derive_attr_parses_traits() {
        match decl_ast!("#[derive(Show, Eq)] class Cell { value: int }") {
            Expression::Class { name, attrs, .. } => {
                assert_eq!(name, "Cell");
                assert_eq!(attrs.len(), 1);
                assert!(matches!(
                    &attrs[0].args,
                    ast::AttrArgs::Idents(v) if v == &["Show", "Eq"]
                ));
            }
            other => panic!("expected Class, got {:?}", other),
        }
    }

    /// Enum without attributes has an empty attrs list.
    #[test]
    fn enum_without_attrs_has_empty_list() {
        match decl_ast!("enum Point { Origin }") {
            Expression::EnumDecl { attrs, .. } => assert!(attrs.is_empty()),
            other => panic!("expected EnumDecl, got {:?}", other),
        }
    }

    #[test]
    fn signature_only_fn_is_parse_error() {
        assert!(
            Pratt::default()
                .declaration()
                .parse("fn strlen(string s) -> int;")
                .into_result()
                .is_err(),
            "orphan signature-only fn must be a parse error"
        );
    }

    #[test]
    fn test_attr_on_fn_parses() {
        match decl_ast!("#[test(\"desc\")] fn foo() { return; }") {
            Expression::Function {
                docs: _,
                attrs, name, body, ..
            } => {
                assert_eq!(name, "foo");
                assert!(body.is_some());
                assert_eq!(attrs.len(), 1);
                assert!(matches!(&attrs[0].args, ast::AttrArgs::String("desc")));
                let body = body.as_ref().unwrap();
                match body.1.as_ref() {
                    Expression::Block(stmts) => {
                        assert!(matches!(
                            stmts[0].1.as_ref(),
                            Expression::Statement(inner)
                                if matches!(
                                    inner.1.as_ref(),
                                    Expression::Return(r)
                                        if matches!(r.1.as_ref(), Expression::Tuple(t) if t.is_empty())
                                )
                        ));
                    }
                    other => panic!("expected Block, got {:?}", other),
                }
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    #[test]
    fn attr_body_target_call_is_spread() {
        match decl_ast!(
            "attr log<T>(fn(...args) -> T target, string message, ...args) -> T { return target(...args); }"
        ) {
            Expression::AttrDecl { body, .. } => match body.1.as_ref() {
                Expression::Block(stmts) => {
                    let ret = &stmts[0];
                    let call = match ret.1.as_ref() {
                        Expression::Statement(inner) => match inner.1.as_ref() {
                            Expression::Return(inner) => match inner.1.as_ref() {
                                Expression::Call { .. } => inner,
                                Expression::Expr(call) => call,
                                other => panic!("expected Call in return, got {:?}", other),
                            },
                            Expression::Expr(call) => call,
                            other => panic!("expected Return/Expr, got {:?}", other),
                        },
                        other => panic!("expected Statement, got {:?}", other),
                    };
                    match call.1.as_ref() {
                        Expression::Call { name, args } => {
                            assert!(matches!(name.1.as_ref(), Expression::Identifier("target")));
                            let args = args.as_ref().expect("args");
                            assert_eq!(args.len(), 1);
                            assert!(matches!(
                                args[0].1.as_ref(),
                                Expression::Spread(inner)
                                    if matches!(inner.1.as_ref(), Expression::Identifier("args"))
                            ));
                        }
                        other => panic!("expected Call, got {:?}", other),
                    }
                }
                other => panic!("expected Block body, got {:?}", other),
            },
            other => panic!("expected AttrDecl, got {:?}", other),
        }
    }

    #[test]
    fn attr_decl_parses() {
        match decl_ast!(
            "attr log<T>(fn(...args) -> T target, string message, ...args) -> T { return target(...args); }"
        ) {
            Expression::AttrDecl { name, .. } => assert_eq!(name, "log"),
            other => panic!("expected AttrDecl, got {:?}", other),
        }
    }

    #[test]
    fn call_site_spread_parses() {
        match decl_ast!("fn main() { pair_sum(...(1, 2)); }") {
            Expression::Function {
                docs: _,
                body: Some(body), ..
            } => match body.1.as_ref() {
                Expression::Block(items) => {
                    let call = match items[0].1.as_ref() {
                        Expression::Statement(inner) => match inner.1.as_ref() {
                            Expression::ExprStatement(call) => call,
                            Expression::Expr(call) => call,
                            other => panic!("expected expr statement, got {:?}", other),
                        },
                        Expression::ExprStatement(call) => call,
                        other => panic!("expected Statement, got {:?}", other),
                    };
                    let call = match call.1.as_ref() {
                        Expression::Call { .. } => call,
                        Expression::Expr(inner) => inner,
                        other => panic!("expected Call, got {:?}", other),
                    };
                    match call.1.as_ref() {
                        Expression::Call {
                            args: Some(args), ..
                        } => {
                            assert_eq!(args.len(), 1);
                            assert!(matches!(args[0].1.as_ref(), Expression::Spread(_)));
                        }
                        other => panic!("expected Call, got {:?}", other),
                    }
                }
                other => panic!("expected Block, got {:?}", other),
            },
            other => panic!("expected Function, got {:?}", other),
        }
    }

    #[test]
    fn stacked_attrs_parse() {
        match decl_ast!("#[derive(Show)] #[test] fn foo() { return; }") {
            Expression::Function { attrs, .. } => {
                assert_eq!(attrs.len(), 2);
                assert_eq!(attrs[0].name, "derive");
                assert_eq!(attrs[1].name, "test");
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    #[test]
    fn parse_unclosed_block_fails() {
        let result = Pratt::default()
            .declaration()
            .parse("fn main() {")
            .into_result();
        assert!(
            result.is_err(),
            "expected unclosed brace to fail, got {:?}",
            result
        );
    }

    #[test]
    fn parse_unclosed_paren_in_call_fails() {
        let result = Pratt::default()
            .declaration()
            .parse("fn main() { foo(1; }")
            .into_result();
        assert!(
            result.is_err(),
            "expected unclosed call paren to fail, got {:?}",
            result
        );
    }

    #[test]
    fn parse_unclosed_string_fails() {
        let result = Pratt::default()
            .declaration()
            .parse(r#"fn main() { write("hi); }"#)
            .into_result();
        assert!(
            result.is_err(),
            "expected unclosed string to fail, got {:?}",
            result
        );
    }

    #[test]
    fn parse_use_with_trailing_double_colon_fails() {
        let result = Pratt::default()
            .declaration()
            .parse("use foo::;")
            .into_result();
        assert!(
            result.is_err(),
            "expected `use foo::;` to fail, got {:?}",
            result
        );
    }

    #[test]
    fn parse_mod_missing_semicolon_fails() {
        // `mod foo` without `;` / body should not parse as a complete declaration.
        let result = Pratt::default()
            .declaration()
            .parse("mod foo")
            .into_result();
        assert!(
            result.is_err(),
            "expected `mod foo` without terminator to fail, got {:?}",
            result
        );
    }

    #[test]
    fn parse_match_arm_missing_arrow_fails() {
        let result = Pratt::default()
            .declaration()
            .parse("fn main() { let x = match 1 { _ 1 }; }")
            .into_result();
        assert!(
            result.is_err(),
            "expected match arm without `=>` to fail, got {:?}",
            result
        );
    }

    #[test]
    fn parse_parenthesized_expr_is_not_one_tuple() {
        // `(1)` must parse as a grouped expression, not a 1-tuple.
        let result = Pratt::default()
            .declaration()
            .parse("fn main() { let x = (1); }")
            .into_result();
        assert!(
            result.is_ok(),
            "expected `(1)` to parse as group, got {:?}",
            result
        );
        let src = result.unwrap().1.to_string();
        assert!(
            !src.contains("(1,)"),
            "group should not render as 1-tuple: {src}"
        );
    }

    #[test]
    fn parse_explicit_one_tuple_requires_trailing_comma() {
        let result = Pratt::default()
            .declaration()
            .parse("fn main() { let x = (1,); }")
            .into_result();
        assert!(
            result.is_ok(),
            "expected `(1,)` to parse as 1-tuple, got {:?}",
            result
        );
    }

    #[test]
    fn parse_invalid_tuple_missing_comma_fails() {
        let result = Pratt::default()
            .declaration()
            .parse("fn main() { let x = (1 2); }")
            .into_result();
        assert!(
            result.is_err(),
            "expected `(1 2)` to fail, got {:?}",
            result
        );
    }

    #[test]
    fn parse_enum_trailing_junk_fails() {
        let result = Pratt::default()
            .declaration()
            .parse("enum E { A B }")
            .into_result();
        assert!(
            result.is_err(),
            "expected missing comma between variants to fail, got {:?}",
            result
        );
    }

    /// P3: `return -1;` is a Return of negated int, not `return - 1` subtraction.
    #[test]
    fn return_negative_literal_parses_as_return() {
        fn unwrap_expr<'a>(expr: &'a Expression<'a>) -> &'a Expression<'a> {
            match expr {
                Expression::Expr(inner) | Expression::Group(inner) => unwrap_expr(inner.1.as_ref()),
                other => other,
            }
        }
        match decl_ast!("fn f() -> int { return -1; }") {
            Expression::Function { body, .. } => {
                fn has_return_negate(expr: &Expression<'_>) -> bool {
                    match expr {
                        Expression::Return(inner) => {
                            matches!(unwrap_expr(inner.1.as_ref()), Expression::Negate(_))
                        }
                        Expression::Block(children)
                        | Expression::Program(children)
                        | Expression::Fragment(children) => {
                            children.iter().any(|c| has_return_negate(c.1.as_ref()))
                        }
                        Expression::Statement(inner)
                        | Expression::ExprStatement(inner)
                        | Expression::Group(inner)
                        | Expression::Expr(inner) => has_return_negate(inner.1.as_ref()),
                        _ => false,
                    }
                }
                assert!(
                    has_return_negate(body.as_ref().expect("function body").1.as_ref()),
                    "expected Return(Negate(...)); got {}",
                    body.as_ref().expect("function body").1
                );
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    #[test]
    fn return_subtraction_still_parses() {
        fn unwrap_expr<'a>(expr: &'a Expression<'a>) -> &'a Expression<'a> {
            match expr {
                Expression::Expr(inner) | Expression::Group(inner) => unwrap_expr(inner.1.as_ref()),
                other => other,
            }
        }
        match decl_ast!("fn f() -> int { return 0 - 1; }") {
            Expression::Function { body, .. } => {
                fn has_return_sub(expr: &Expression<'_>) -> bool {
                    match expr {
                        Expression::Return(inner) => {
                            matches!(unwrap_expr(inner.1.as_ref()), Expression::Sub(_, _))
                        }
                        Expression::Block(children)
                        | Expression::Program(children)
                        | Expression::Fragment(children) => {
                            children.iter().any(|c| has_return_sub(c.1.as_ref()))
                        }
                        Expression::Statement(inner)
                        | Expression::ExprStatement(inner)
                        | Expression::Group(inner)
                        | Expression::Expr(inner) => has_return_sub(inner.1.as_ref()),
                        _ => false,
                    }
                }
                assert!(
                    has_return_sub(body.as_ref().expect("function body").1.as_ref()),
                    "expected Return(Sub(...)); got {}",
                    body.as_ref().expect("function body").1
                );
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    /// P3 sibling: `yield -1;` must be Yield(Negate(...)), not
    /// `Sub(Identifier("yield"), 1)` via expr_statement.
    #[test]
    fn yield_negative_literal_parses_as_yield() {
        fn unwrap_expr<'a>(expr: &'a Expression<'a>) -> &'a Expression<'a> {
            match expr {
                Expression::Expr(inner) | Expression::Group(inner) => unwrap_expr(inner.1.as_ref()),
                other => other,
            }
        }
        match decl_ast!("async fn coro() { yield -1; }") {
            Expression::Function { body, .. } => {
                fn has_yield_negate(expr: &Expression<'_>) -> bool {
                    match expr {
                        Expression::Yield(inner) => {
                            matches!(unwrap_expr(inner.1.as_ref()), Expression::Negate(_))
                        }
                        Expression::Block(children)
                        | Expression::Program(children)
                        | Expression::Fragment(children) => {
                            children.iter().any(|c| has_yield_negate(c.1.as_ref()))
                        }
                        Expression::Statement(inner)
                        | Expression::ExprStatement(inner)
                        | Expression::Group(inner)
                        | Expression::Expr(inner) => has_yield_negate(inner.1.as_ref()),
                        _ => false,
                    }
                }
                assert!(
                    has_yield_negate(body.as_ref().expect("function body").1.as_ref()),
                    "expected Yield(Negate(...)); got {}",
                    body.as_ref().expect("function body").1
                );
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    #[test]
    fn nested_array_type_annotation_parses() {
        use chumsky::Parser;

        let option_array = Pratt::default()
            .type_annotation()
            .parse("[Option<int>]")
            .into_result()
            .expect("parse [Option<int>] failed");
        match option_array.1.as_ref() {
            Expression::Array(items) if items.len() == 1 => match items[0].1.as_ref() {
                Expression::TypeApp { name, args } => {
                    assert_eq!(*name, "Option");
                    assert_eq!(args.len(), 1);
                    assert!(matches!(args[0].1.as_ref(), Expression::Type("int")));
                }
                other => panic!("expected TypeApp element, got {:?}", other),
            },
            other => panic!("expected [Option<int>], got {:?}", other),
        }

        let nested = Pratt::default()
            .type_annotation()
            .parse("[[int; N]; M]")
            .into_result()
            .expect("parse [[int; N]; M] failed");
        match nested.1.as_ref() {
            Expression::Array(outer) if outer.len() == 2 => {
                match outer[0].1.as_ref() {
                    Expression::Array(inner) if inner.len() == 2 => {
                        assert!(matches!(inner[0].1.as_ref(), Expression::Type("int")));
                        assert!(matches!(inner[1].1.as_ref(), Expression::Type("N")));
                    }
                    other => panic!("expected inner [int; N], got {:?}", other),
                }
                assert!(matches!(outer[1].1.as_ref(), Expression::Type("M")));
            }
            other => panic!("expected [[int; N]; M], got {:?}", other),
        }
    }
