// Runtime "negative" paths that must remain well-defined (soft Err, not abort).
fn boom() {
    raise "x";
}

fn wrap() {
    boom()?;
    return 1;
}

fn inner() {
    raise "inner";
}

fn outer() {
    let _ = inner()?;
    return 0;
}

test("assert false is Err not panic") {
    let r = assert(false);
    match r {
        Result::Ok(_) => raise "expected Err from assert(false)",
        Result::Err(_) => assert(true)?,
    }
}

test("assert message preserved") {
    let msg = match assert(false, "boom") {
        Result::Ok(_) => "",
        Result::Err(e) => e,
    };
    assert(msg == "boom")?;
}

test("raise propagates through ?") {
    let r = wrap();
    match r {
        Result::Ok(_) => raise "expected Err",
        Result::Err(e) => assert(e == "x")?,
    }
}

test("option none coalesce") {
    assert((Option::None ?? 0) == 0)?;
}

test("result err coalesce swallows") {
    assert((Result::Err("e") ?? 5) == 5)?;
}

test("match err arm taken") {
    let r = Result::Err("nope");
    let v = match r {
        Result::Ok(n) => n,
        Result::Err(_) => 0 - 1,
    };
    assert(v == 0 - 1)?;
}

test("failed assert does not poison next assert") {
    let _ = assert(false);
    assert(true)?;
}

test("double question mark on nested result") {
    match outer() {
        Result::Ok(_) => raise "expected Err",
        Result::Err(e) => assert(e == "inner")?,
    }
}
