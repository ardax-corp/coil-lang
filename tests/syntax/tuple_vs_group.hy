// `(e)` is a parenthesized expression, `(e,)` a 1-tuple and `(a, b)` a
// tuple; grouping overrides precedence anywhere in an expression.
use tests::assert;

test("group is the inner value") {
    let g = 1;
    assert!(g + 1 == 2);
    assert!((typeof g) == "int");
    assert!(((((3)))) == 3);
}

test("one-tuple needs a trailing comma") {
    let t = (5,);
    assert!(t[0] == 5);
    assert!(t.len() == 1);
    let u = ("x",);
    assert!(u[0] == "x");
}

test("tuples of several elements") {
    let p = (1, "two", 3.0, true);
    assert!(p.len() == 4);
    assert!(p[1] == "two");
    assert!(p[3]);
    let nested = ((1, 2), (3,));
    assert!(nested[1][0] == 3);
}

test("grouping overrides precedence") {
    assert!((2 + 3) * 4 == 20);
    assert!(2 + 3 * 4 == 14);
    assert!((2 ** 3) ** 2 == 64);
    assert!(-(2 - 5) == 3);
    assert!((1 << 2) | 1 == 5);
    assert!((true || false) && false == false);
}
