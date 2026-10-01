// #597: `==` between an `Option` / `Result` value and a constructor literal
// unifies the literal with the other operand, so payloads compare by type
// (strings by content) and a typed operand type-checks.

fn join(string a, string b) -> string {
    return a + "/" + b;
}

fn computed() -> Result<string, string> {
    return Result::Ok(join("a", "coil"));
}

fn literal() -> Result<string, string> {
    return Result::Ok("a/coil");
}

fn failed() -> Result<string, string> {
    return Result::Err(join("e", "x"));
}

fn count() -> Result<int, string> {
    return Result::Ok(3);
}

test("result compares with a constructor literal") {
    assert(computed() == Result::Ok("a/coil"))?;
    assert(literal() == Result::Ok("a/coil"))?;
    assert(Result::Ok("a/coil") == computed())?;
    assert(computed() != Result::Ok("other"))?;
    assert(count() == Result::Ok(3))?;
    assert(failed() == Result::Err("e/x"))?;
    assert(failed() != Result::Ok("e/x"))?;
}

test("typed operands") {
    let a: Result<string, string> = Result::Ok("x");
    assert(a == Result::Ok("x"))?;
    assert(a != Result::Err("x"))?;
    let o: Option<string> = Option::Some("a/coil");
    assert(o == Option::Some(join("a", "coil")))?;
    assert(o != Option::None)?;
    let n: Option<int> = Option::None;
    assert(n == Option::None)?;
    assert(n != Option::Some(0))?;
}
