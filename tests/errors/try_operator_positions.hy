// `?` binds tightest (postfix), so it works on each operand of an
// arithmetic expression, inside a constructor in a `return`, once per layer
// of a nested Result, and on the right of `??` (`a?.x ?? b?`), where it
// runs only when the left side is `None`.
use tests::assert;

class Point {
    pub x: int,
}

fn num(int n) -> Option<int> {
    if n < 0 {
        return Option::None;
    }
    return Option::Some(n);
}

fn sum_both(int a, int b) -> Option<int> {
    return Option::Some(num(a)? + num(b)? * 10);
}

fn nested(Result<Result<int, string>, string> r) -> Result<int, string> {
    let inner = r?;
    return inner? + 1;
}

fn pick(Option<Point> a, Option<int> b) -> Option<int> {
    return Option::Some(a?.x ?? b?);
}

test("? on both operands") {
    assert!(sum_both(1, 2) == Option::Some(21));
    assert!(sum_both(0, 0) == Option::Some(0));
    assert!(sum_both(-1, 2) == Option::None);
    assert!(sum_both(1, -2) == Option::None);
}

test("a nested Result unwraps one layer per ?") {
    assert!(nested(Result::Ok(Result::Ok(4))) == Result::Ok(5));
    assert!(nested(Result::Ok(Result::Err("inner"))) == Result::Err("inner"));
    assert!(nested(Result::Err("outer")) == Result::Err("outer"));
}

test("? inside a coalesce chain runs only when needed") {
    let p = Option::Some(new Point(7));
    let none_p: Option<Point> = Option::None;
    assert!(pick(p, Option::Some(1)) == Option::Some(7));
    assert!(pick(p, Option::None) == Option::Some(7));
    assert!(pick(none_p, Option::Some(3)) == Option::Some(3));
    assert!(pick(none_p, Option::None) == Option::None);
}
