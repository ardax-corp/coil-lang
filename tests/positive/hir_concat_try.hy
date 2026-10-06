// A `?` or `??` inside a string `+` lowers from HIR: both operands run into
// temps at depth zero, then the format string goes under them.
fn piece(string s, int n) -> Result<string, string> {
    if n < 0 {
        return Result::Err("neg");
    }
    return Result::Ok(s + "!");
}

fn maybe(int n) -> Option<string> {
    if n > 0 {
        return Option::Some("m");
    }
    return Option::None;
}

fn chain(string s, int n) -> Result<string, string> {
    let out = "<";
    out = out + piece(s, n)? + ">";
    return Result::Ok(out);
}

fn tail(string s, int n) -> Result<string, string> {
    let out = "x";
    return out + piece(s, n)?;
}

fn both(int a, int b) -> Result<string, string> {
    return piece("a", a)? + piece("b", b)?;
}

fn coalesce(int n) -> string {
    let pre = "p";
    return pre + (maybe(n) ?? "none") + ".";
}

fn looped(int n) -> Result<string, string> {
    let out = "";
    let i = 0;
    while i < n {
        out = out + piece("i", i)?;
        i = i + 1;
    }
    return out + piece("end", n)?;
}

test("try inside string concat") {
    assert((chain("a", 1) ?? "err") == "<a!>")?;
    assert((chain("a", -1) ?? "err") == "err")?;
    assert((tail("b", 0) ?? "err") == "xb!")?;
    assert((tail("b", -2) ?? "err") == "err")?;
    assert((both(1, 1) ?? "err") == "a!b!")?;
    assert((both(1, -1) ?? "err") == "err")?;
    assert((both(-1, 1) ?? "err") == "err")?;
}

test("coalesce inside string concat") {
    assert(coalesce(1) == "pm.")?;
    assert(coalesce(0) == "pnone.")?;
}

test("try inside a looped concat") {
    assert((looped(3) ?? "err") == "i!i!i!end!")?;
    assert((looped(0) ?? "err") == "end!")?;
}
