// Function-style macro arguments arrive as source: their text (`str`), kind
// and literal / identifier flags, any number of them through a `Vec<Expr>`;
// output may be an expression, statements, or declarations built with
// `quote type`.
use tests::assert;
use macro_probe::{texts, glued, kinds, count_args, list_of, pick, getter, once};
use fn_macros::square;

static let CALLS: int = 0;

fn counted(int n) -> int {
    CALLS += 1;
    return n;
}

list_of!(Ints, int);

getter!(Gauge, level);

test("argument text is kept as written") {
    assert!(texts!(a + b, f(1, 2), "s") == "a + b|f(1, 2)|\"s\"");
    assert!(texts!(x) == "x");
    // `concat` puts each part on its own line.
    assert!(glued!(ab, cd, 12) == "ab\ncd\n12");
}

test("argument kinds and flags") {
    assert!(
        kinds!(1, "s", x, a::b, f(2), 1 + 2) ==
        "literal/yn literal/yn ident/ny path/nn call/nn other/nn",
    );
}

test("a Vec<Expr> parameter counts zero or more arguments") {
    assert!(count_args!() == 0);
    assert!(count_args!(1) == 1);
    assert!(count_args!(a, b, c, d, e) == 5);
}

test("a pasted argument is evaluated each time it appears") {
    CALLS = 0;
    let v = square!(counted(3));
    assert!(v == 9);
    assert!(CALLS == 2);
}

test("binding the argument once in the output evaluates it once") {
    CALLS = 0;
    let v = once!(counted(4));
    assert!(v == 8);
    assert!(CALLS == 1);
}

test("a type built with quote type") {
    let xs: Ints = Vec::new();
    xs.push(3);
    xs.push(4);
    assert!(xs.len() == 2 && xs[1] == 4);
}

test("an expression macro inside a larger expression") {
    assert!(pick!(true, 1, 2) + pick!(1 > 2, 10, 20) == 21);
    assert!(square!(pick!(false, 0, 3)) == 9);
}

test("top-level output declares a class with a generated method") {
    let g = new Gauge(5);
    assert!(g.get_level() == 5);
    g.level = 6;
    assert!(g.get_level() == 6);
}
