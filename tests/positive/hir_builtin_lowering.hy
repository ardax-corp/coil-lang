// Test bodies, `assert` and host builtins lower from HIR. A test returns
// its `Result<(), string>` boxed, `assert` yields the niche word, and a
// host call pushes its native id under the arguments. The language
// harness also runs under `--hir`, so each case pins both codegens.
fn check(int x) -> string {
    return match assert(x > 0, "not positive") {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
}

fn hyp(float a, float b) -> float {
    return sqrt(a * a + b * b);
}

fn spread(float x) -> float {
    return 1.0 + floor(x) * 2.0;
}

fn parse(int x) -> Result<int, string> {
    if x < 0 {
        return Result::Err("negative");
    }
    return Result::Ok(x * 2);
}

fn label(int base, Result<int, string> r) -> int {
    return match r {
        Result::Ok(n) => base + n,
        Result::Err(_) => base - 1,
    };
}

test("assert as a value") {
    assert(check(3) == "ok")?;
    assert(check(0) == "not positive")?;
}

test("host calls nest and sit above operands") {
    assert(hyp(3.0, 4.0) == 5.0)?;
    assert(spread(2.5) == 5.0)?;
    assert((ord("a")? as int) + 1 == (ord("b")? as int))?;
}

test("early return from a test") {
    if hyp(6.0, 8.0) == 10.0 {
        return;
    }
    assert(false, "unreachable")?;
}

test("a pair result boxes above a live operand") {
    assert(label(100, parse(4)) == 108)?;
    assert(label(100, parse(0 - 4)) == 99)?;
}
