// #593: a `?` inside a call argument used to leave its value on the stack
// while the other args were stored into temps over it, so
// `contains(name()?, "--root")` compared "--root" with itself.
use text::contains;

fn name() -> Result<string, string> {
    return Result::Ok("coil");
}

fn fail() -> Result<string, string> {
    return Result::Err("no name");
}

fn some_len() -> Option<int> {
    return Option::Some(3);
}

fn join(string a, string b) -> string {
    return a + "/" + b;
}

fn in_if() -> Result<bool, string> {
    if contains(name()?, "--root") {
        return Result::Ok(true);
    }
    return Result::Ok(false);
}

fn in_nested_if() -> Result<bool, string> {
    if contains(join(name()?, "lsp"), "--root") {
        return Result::Ok(true);
    }
    return Result::Ok(false);
}

fn in_while() -> Result<int, string> {
    let n = 0;
    while contains(join(name()?, "lsp"), "--root") && n < 3 {
        n = n + 1;
    }
    return Result::Ok(n);
}

fn in_let() -> Result<string, string> {
    let joined = join("a", name()?);
    return Result::Ok(joined);
}

fn first_arg() -> Result<string, string> {
    return Result::Ok(join(name()?, "b"));
}

fn propagates() -> Result<bool, string> {
    if contains(fail()?, "x") {
        return Result::Ok(true);
    }
    return Result::Ok(false);
}

fn option_arg() -> Option<int> {
    return Option::Some(some_len()? * 10 + 1);
}

fn ok_bool(Result<bool, string> r) -> string {
    return match r {
        Result::Ok(b) => {
            if b {
                return "true";
            }
            "false"
        },
        Result::Err(e) => "err: " + e,
    };
}

fn ok_str(Result<string, string> r) -> string {
    return match r {
        Result::Ok(v) => v,
        Result::Err(e) => "err: " + e,
    };
}

fn ok_int(Result<int, string> r) -> int {
    return match r {
        Result::Ok(n) => n,
        Result::Err(_) => -1,
    };
}

test("? in an if condition's call argument") {
    assert(ok_bool(in_if()) == "false", "contains(\"coil\", \"--root\") is false")?;
    assert(ok_bool(in_nested_if()) == "false", "nested call argument")?;
}

test("? in a while condition's call argument") {
    assert(ok_int(in_while()) == 0, "loop body never runs")?;
}

test("? keeps argument order") {
    assert(ok_str(in_let()) == "a/coil", "second argument")?;
    assert(ok_str(first_arg()) == "coil/b", "first argument")?;
}

test("? still returns early on Err") {
    assert(ok_bool(propagates()) == "err: no name", "Err propagates")?;
}

test("? on Option in an argument") {
    let got = match option_arg() {
        Option::Some(n) => n,
        Option::None => -1,
    };
    assert(got == 31, "Some(3 * 10 + 1)")?;
}
