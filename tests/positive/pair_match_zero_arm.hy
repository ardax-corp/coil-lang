// A two-slot match whose arms are "the payload" and "`0`" folds to the
// payload only when the `0` arm is a unit variant (`None`). `Ok(_)` still
// carries a payload, so `Ok(_) => 0` must not read it.
fn f(int x) -> Result<int, int> {
    if x < 0 {
        raise 3;
    }
    return x;
}

fn g(int x) -> Option<int> {
    if x < 0 {
        return Option::None;
    }
    return Option::Some(x);
}

test("Ok(_) => 0 ignores the Ok payload") {
    let a = match f(5) {
        Result::Ok(_) => 0,
        Result::Err(e) => e,
    };
    let b = match f(-1) {
        Result::Ok(v) => v,
        Result::Err(_) => 0,
    };
    assert(a == 0)?;
    assert(b == 0)?;
}

test("Some(x) => x, None => 0 still reads the payload") {
    let a = match g(5) {
        Option::Some(x) => x,
        Option::None => 0,
    };
    let b = match g(-1) {
        Option::None => 0,
        Option::Some(x) => x,
    };
    assert(a == 5)?;
    assert(b == 0)?;
}
