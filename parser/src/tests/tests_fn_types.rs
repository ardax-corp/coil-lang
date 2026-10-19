    //! `fn(T x) -> R` / `fn(T) -> R` is one function-type grammar shared by
    //! every type position (#1389, #1391).
    use super::*;
    use ast::Expression;

    fn count_fn_sigs(expr: &Expression<'_>) -> usize {
        let mut n = usize::from(matches!(expr, Expression::TypeFnSig { .. }));
        expr.for_each_child(&mut |c: &Output<'_>| n += count_fn_sigs(c.1.as_ref()));
        n
    }

    fn fn_sigs_in(src: &str) -> usize {
        let ast = Pratt::default().parse(src).unwrap_or_else(|e| panic!("parse failed: {e:?}"));
        count_fn_sigs(ast.1.as_ref())
    }

    #[test]
    fn fn_type_parses_in_every_type_position() {
        let positions = [
            "type IntFn = fn(int x) -> int;",
            "type IntFn = fn(int) -> int;",
            "fn apply(fn(int x) -> int f, int v) -> int { return f(v); }",
            "fn apply(fn(int) -> int f, int v) -> int { return f(v); }",
            "fn apply((fn(int x) -> int) f, int v) -> int { return f(v); }",
            "fn mk() -> fn(int x) -> int { return fn (int y) => y; }",
            "class Holder { op: fn(int) -> int, }",
            "fn main() { let g: fn(int) -> int = fn (int x) => x; }",
            "fn main() { let fs: Vec<fn(int) -> int> = []; }",
        ];
        for src in positions {
            assert_eq!(fn_sigs_in(src), 1, "expected one fn type in `{src}`");
        }
    }

    #[test]
    fn nested_fn_type_parameters_parse() {
        let src = "fn twice(fn(fn(int) -> int g, int v) -> int run) -> int { return 0; }";
        assert_eq!(fn_sigs_in(src), 2);
    }

    #[test]
    fn arrow_spelling_still_parses_in_parameters() {
        let src = "fn apply(int -> int f, (int -> int) -> int g) -> int { return f(1); }";
        assert_eq!(fn_sigs_in(src), 0);
    }

    #[test]
    fn fn_types_format_round_trip() {
        let src = "type F = fn(int, string) -> int;\n\nfn apply(fn(int x) -> int f, (int -> int) -> int g) -> int {\n    return f(1);\n}\n";
        let out = crate::fmt::format_source(src).expect("format");
        assert_eq!(out, src);
        assert_eq!(crate::fmt::format_source(&out).expect("reformat"), out);
    }

    fn fn_sig_texts(expr: &Expression<'_>, out: &mut Vec<String>) {
        if matches!(expr, Expression::TypeFnSig { .. }) {
            out.push(expr.to_string());
        }
        expr.for_each_child(&mut |c: &Output<'_>| fn_sig_texts(c.1.as_ref(), out));
    }

    #[test]
    fn nullary_fn_type_parses_in_every_type_position() {
        let positions = [
            "type Thunk = fn() -> int;",
            "fn at(fn() -> int f) -> int { return f(); }",
            "fn mk() -> fn() -> int { return fn () => 1; }",
            "class Lazy { make: fn() -> int, }",
            "fn main() { let g: fn() -> int = fn () => 2; }",
            "fn main() { let fs: Vec<fn() -> int> = []; }",
        ];
        for src in positions {
            assert_eq!(fn_sigs_in(src), 1, "expected one fn type in `{src}`");
        }
    }

    #[test]
    fn nullary_fn_types_format_round_trip() {
        let src = "type T = fn() -> int;\n\nfn at(fn() -> int f, fn(fn() -> int g) -> int run) -> fn() -> int {\n    return f;\n}\n";
        let out = crate::fmt::format_source(src).expect("format");
        assert_eq!(out, src);
        assert_eq!(crate::fmt::format_source(&out).expect("reformat"), out);
    }

    #[test]
    fn fn_types_display_with_their_parameter_list() {
        let src = "fn f(fn() -> int a, fn(int, string) -> int b, fn(int x) -> int c) -> int { return 0; }";
        let ast = Pratt::default().parse(src).unwrap_or_else(|e| panic!("parse failed: {e:?}"));
        let mut texts = Vec::new();
        fn_sig_texts(ast.1.as_ref(), &mut texts);
        assert_eq!(texts, ["fn() -> int", "fn(int, string) -> int", "fn(int x) -> int"]);
    }
