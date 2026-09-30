// Function-style macros from `examples/src/fn_macros.hy` in expression,
// statement and item position.
use fn_macros::{square, check, sum, quad, describe, counter, swap};

// Item position: the output is declarations.
counter!(Clicks);

fn id(int x) -> int {
    return x;
}

test("expression position keeps each argument whole") {
    assert(square!(3) == 9)?;
    assert(square!(1 + 2) == 9)?;
    assert(square!(id(4)) - 1 == 15)?;
}

test("a last Vec<Expr> parameter takes the rest") {
    assert(sum!() == 0)?;
    assert(sum!(7) == 7)?;
    assert(sum!(1, 2, 3 * 4) == 15)?;
}

test("macros in a macro's output expand next round") {
    assert(quad!(2) == 16)?;
    assert(square!(square!(3)) == 81)?;
}

test("arguments arrive with their source text and kind") {
    assert(describe!(id(1)) == "call: id(1)")?;
    assert(describe!(x) == "ident: x")?;
    assert(describe!(a.b) == "path: a.b")?;
    assert(describe!(1 + 2) == "other: 1 + 2")?;
    assert(describe!("hi") == "literal: \"hi\"")?;
    assert(describe!(-3) == "literal: -3")?;
}

test("statement position: the output is statements") {
    check!(square!(2) == 4);
    let a = 1;
    let b = 2;
    swap!(a, b);
    assert(a == 2 && b == 1)?;
    // Hygiene: each expansion's `tmp` is its own.
    let tmp = 100;
    swap!(a, b);
    swap!(a, b);
    assert(a == 2 && b == 1 && tmp == 100)?;
}

test("item position: the output is declarations") {
    let c = new Clicks(0);
    c.bump();
    assert(c.bump() == 2)?;
}
