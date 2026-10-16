// #1300: a function returning `Result<Result<T, E>, E2>` can return a whole
// value of that type: a parameter, `Result::Ok(Result::…)`, or a `match`
// that rebuilds it. `Result::Ok(x)` of a plain payload still wraps.
use tests::assert;

fn wrap(Result<Result<int, string>, string> r) -> Result<Result<int, string>, string> {
    return r;
}

fn make(int n) -> Result<Result<int, string>, string> {
    if n == 0 {
        return Result::Ok(Result::Err("in"));
    }
    if n < 0 {
        return Result::Err("out");
    }
    return Result::Ok(Result::Ok(n));
}

fn rebuild(Result<Result<int, string>, string> r) -> Result<Result<int, string>, string> {
    return match r {
        Result::Ok(i) => Result::Ok(i),
        Result::Err(e) => Result::Err(e),
    };
}

fn parse(int n) -> Result<int, string> {
    if n < 0 {
        return Result::Err("neg");
    }
    return Result::Ok(n);
}

fn payload(int n) -> Result<Result<int, string>, string> {
    let v = parse(n)?;
    return Result::Ok(v);
}

fn describe(Result<Result<int, string>, string> r) -> string {
    return match r {
        Result::Ok(Result::Ok(n)) => string::format("ok ok %i", n),
        Result::Ok(Result::Err(e)) => "ok err " + e,
        Result::Err(e) => "err " + e,
    };
}

test("a parameter of the nested type returns as is") {
    assert!(describe(wrap(Result::Err("e"))) == "err e");
    assert!(describe(wrap(Result::Ok(Result::Ok(3)))) == "ok ok 3");
    assert!(describe(wrap(Result::Ok(Result::Err("x")))) == "ok err x");
}

test("explicit nested constructs") {
    assert!(describe(make(1)) == "ok ok 1");
    assert!(describe(make(0)) == "ok err in");
    assert!(describe(make(-1)) == "err out");
}

test("a match that rebuilds the value") {
    assert!(describe(rebuild(make(4))) == "ok ok 4");
    assert!(describe(rebuild(make(0))) == "ok err in");
    assert!(describe(rebuild(make(-1))) == "err out");
}

test("an Ok of the payload still wraps") {
    assert!(describe(payload(5)) == "ok ok 5");
    assert!(describe(payload(-5)) == "err neg");
}
