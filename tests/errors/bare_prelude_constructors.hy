// Bare `Some` / `None` / `Ok` / `Err` in expressions are sugar for the
// prelude constructors while the case name is unique in scope.
use tests::assert;

fn half(int n) -> Option<int> {
    if n % 2 == 1 {
        return None;
    }
    return Some(n / 2);
}

fn parse(int n) -> Result<int, string> {
    if n < 0 {
        return Err("neg");
    }
    return Ok(n);
}

test("bare constructors build prelude values") {
    assert!(half(4) == Option::Some(2));
    assert!(half(3) == Option::None);
    assert!(parse(1) == Result::Ok(1));
    assert!(parse(0) == Result::Ok(0));
    assert!(parse(-1) == Result::Err("neg"));
}

test("bare values compare equal to qualified ones") {
    assert!(Some(1) == Option::Some(1));
    let n: Option<int> = None;
    assert!(n == Option::None);
    let e: Result<int, string> = Err("x");
    assert!(e == Result::Err("x"));
    assert!((half(8) ?? 0) == 4);
}
