// A function that uses `raise` is in result mode: `return v` becomes
// `Ok(v)`, `raise e` returns `Err(e)`, explicit `Ok` / `Err` returns are not
// wrapped twice, and the error type may be a user enum or a class.
use tests::assert;

enum ParseError {
    Empty,
    TooBig(int),
}

class Failure {
    pub code: int,
}

fn checked(int n) {
    if n < 0 {
        raise "negative";
    }
    return n * 2;
}

fn either(int n) -> Result<int, string> {
    if n == 0 {
        return Result::Err("zero");
    }
    if n == 1 {
        return Result::Ok(100);
    }
    if n < 0 {
        raise "negative";
    }
    return n;
}

fn parse(int len, int value) -> Result<int, ParseError> {
    if len == 0 {
        raise ParseError::Empty;
    }
    if value > 99 {
        raise ParseError::TooBig(value);
    }
    return value;
}

fn fail_with(int code) -> Result<string, Failure> {
    if code != 0 {
        raise new Failure(code);
    }
    return "fine";
}

fn first_negative(Vec<int> xs) {
    for x in xs {
        if x < 0 {
            raise x;
        }
    }
    return xs.len();
}

test("plain returns are wrapped in Ok") {
    assert!(checked(4) == Result::Ok(8));
    assert!(checked(-1) == Result::Err("negative"));
    assert!(checked(0) == Result::Ok(0));
}

test("explicit Ok and Err are not wrapped again") {
    assert!(either(0) == Result::Err("zero"));
    assert!(either(1) == Result::Ok(100));
    assert!(either(-5) == Result::Err("negative"));
    assert!(either(7) == Result::Ok(7));
}

test("a user enum as the error type") {
    let a = match parse(0, 1) {
        Result::Err(ParseError::Empty) => "empty",
        Result::Err(ParseError::TooBig(_)) => "big",
        Result::Ok(_) => "ok",
    };
    assert!(a == "empty");
    let b = match parse(3, 500) {
        Result::Err(ParseError::TooBig(v)) => v,
        default => 0,
    };
    assert!(b == 500);
    assert!((parse(2, 42).ok() ?? -1) == 42);
    assert!((parse(2, 99).ok() ?? -1) == 99);
    assert!((parse(2, 100).ok() ?? -1) == -1);
}

test("a class as the error type") {
    let code = match fail_with(7) {
        Result::Err(f) => f.code,
        Result::Ok(_) => 0,
    };
    assert!(code == 7);
    assert!((fail_with(0).ok() ?? "") == "fine");
}

test("raise from inside a loop") {
    assert!(first_negative([1, 2, 3]) == Result::Ok(3));
    assert!(first_negative([0]) == Result::Ok(1));
    assert!(first_negative([1, -4, -5]) == Result::Err(-4));
}
