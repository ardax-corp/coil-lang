    use super::*;
    use chumsky::Parser;

    /// Unwrap the outer `Expression::Expr` wrapper the Pratt root often adds.
    fn unwrap_expr<'a>(expr: &'a Expression<'a>) -> &'a Expression<'a> {
        match expr {
            Expression::Expr((_, inner)) => unwrap_expr(inner),
            other => other,
        }
    }

    fn parse_expr(src: &str) -> Box<Expression<'_>> {
        Pratt::default()
            .expr()
            .parse(src)
            .into_result()
            .unwrap_or_else(|e| panic!("parse failed for `{}`: {:?}", src, e))
            .1
    }

    macro_rules! expr {
        ($case: expr) => {{ parse_expr($case).to_string() }};
    }

    macro_rules! same_try {
        ($case: literal) => {
            assert_eq!($case.to_string(), expr!($case));
        };
    }

    #[test]
    fn raise_parses_as_raise_expression() {
        match unwrap_expr(parse_expr("raise \"boom\"").as_ref()) {
            Expression::Raise(inner) => assert_eq!(inner.1.to_string(), "\"boom\""),
            other => panic!("expected Raise, got {:?}", other),
        }
    }

    #[test]
    fn panic_parses_as_panic_expression() {
        match unwrap_expr(parse_expr("panic \"boom\"").as_ref()) {
            Expression::Panic(inner) => assert_eq!(inner.1.to_string(), "\"boom\""),
            other => panic!("expected Panic, got {:?}", other),
        }
    }

    #[test]
    fn typeof_parses_as_typeof_expression() {
        match unwrap_expr(parse_expr("typeof x").as_ref()) {
            Expression::TypeOf(inner) => assert_eq!(inner.1.to_string(), "x"),
            other => panic!("expected TypeOf, got {:?}", other),
        }
        match unwrap_expr(parse_expr("typeof (1 + 2)").as_ref()) {
            Expression::TypeOf(_) => {}
            other => panic!("expected TypeOf of group, got {:?}", other),
        }
    }

    #[test]
    fn comptime_binds_like_unary_minus() {
        match unwrap_expr(parse_expr("comptime f(x)? + 1").as_ref()) {
            Expression::Add(lhs, _) => match lhs.1.as_ref() {
                Expression::Comptime(inner) => assert_eq!(inner.1.to_string(), "f(x)?"),
                other => panic!("expected Comptime, got {:?}", other),
            },
            other => panic!("expected Add, got {:?}", other),
        }
        match unwrap_expr(parse_expr("comptime (1 + 2)").as_ref()) {
            Expression::Comptime(_) => {}
            other => panic!("expected Comptime of group, got {:?}", other),
        }
    }

    #[test]
    fn postfix_try_parses_to_try() {
        same_try!("x?");
        assert!(matches!(
            unwrap_expr(parse_expr("x?").as_ref()),
            Expression::Try(_)
        ));
    }

    #[test]
    fn coalesce_parses_and_is_right_associative() {
        assert_eq!(expr!("a ?? b ?? c"), "a ?? b ?? c");
        match unwrap_expr(parse_expr("a ?? b ?? c").as_ref()) {
            Expression::Coalesce(lhs, rhs) => {
                assert!(matches!(
                    unwrap_expr(lhs.1.as_ref()),
                    Expression::Identifier("a")
                ));
                assert!(matches!(
                    unwrap_expr(rhs.1.as_ref()),
                    Expression::Coalesce(_, _)
                ));
            }
            other => panic!("expected right-assoc Coalesce, got {:?}", other),
        }
    }

    #[test]
    fn optional_access_parses_to_optional_access() {
        match unwrap_expr(parse_expr("x?.y").as_ref()) {
            Expression::OptionalAccess(recv, field) => {
                assert!(matches!(
                    unwrap_expr(recv.1.as_ref()),
                    Expression::Identifier("x")
                ));
                assert_eq!(*field, "y");
            }
            other => panic!("expected OptionalAccess, got {:?}", other),
        }
    }

    #[test]
    fn try_and_optional_access_bind_tighter_than_coalesce() {
        assert_eq!(expr!("a? ?? b"), "a? ?? b");
        assert_eq!(expr!("a?.x ?? b"), "a?.x ?? b");
    }

    #[test]
    fn coalesce_binds_tighter_than_assignment() {
        // `a = b ?? c` is Assignment(a, Coalesce(b, c)), not Coalesce(Assign(...), c).
        match unwrap_expr(parse_expr("a = b ?? c").as_ref()) {
            Expression::Assignment(_, rhs) => {
                assert!(matches!(
                    unwrap_expr(rhs.1.as_ref()),
                    Expression::Coalesce(_, _)
                ));
            }
            other => panic!("expected Assignment with Coalesce rhs, got {:?}", other),
        }
    }

    #[test]
    fn coalesce_binds_looser_than_or() {
        assert_eq!(expr!("a || b ?? c"), "a || b ?? c");
        match unwrap_expr(parse_expr("a || b ?? c").as_ref()) {
            Expression::Coalesce(lhs, _) => {
                assert!(matches!(unwrap_expr(lhs.1.as_ref()), Expression::Or(_, _)));
            }
            other => panic!("expected Coalesce of Or, got {:?}", other),
        }
    }

    #[test]
    fn error_handling_display_round_trips() {
        same_try!("raise 1");
        same_try!("x?");
        same_try!("a ?? b");
        same_try!("o?.f");
    }

    /// `x? .y` is a member access on the `?` result (#1540); only the
    /// unbroken `?.` digraph is optional access.
    #[test]
    fn spaced_dot_after_try_is_access_on_try() {
        for src in ["x? .y", "x?\n    .y"] {
            match unwrap_expr(parse_expr(src).as_ref()) {
                Expression::Access(recv, field) => {
                    assert_eq!(*field, "y");
                    assert!(matches!(
                        unwrap_expr(recv.1.as_ref()),
                        Expression::Try(_)
                    ));
                }
                other => panic!("{src:?}: expected Access(Try), got {:?}", other),
            }
        }
        assert!(matches!(
            unwrap_expr(parse_expr("x?.y").as_ref()),
            Expression::OptionalAccess(..)
        ));
    }
