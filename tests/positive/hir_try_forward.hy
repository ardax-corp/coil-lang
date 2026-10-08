// A returned `e?` of the function's own type forwards `e` as is, and a
// `?` miss into the same pair returns the payload with its tag pushed
// back.
fn step(int n) -> Result<int, int> {
    if n == 0 {
        return Result::Err(-1);
    }
    return Result::Ok(n);
}

fn pick(int n) -> Option<int> {
    if n == 0 {
        return Option::None;
    }
    return Option::Some(n * 2);
}

fn forward(int n) -> Result<int, int> {
    return step(n)?;
}

fn rewrap(int n) -> Result<int, int> {
    return Result::Ok(step(n)?);
}

fn some_again(int n) -> Option<int> {
    return Option::Some(pick(n)?);
}

fn chain(int n) -> Result<int, int> {
    let a = step(n)?;
    let b = step(a - 1)?;
    return Result::Ok(a + b);
}

fn widen(int n) -> Result<float, int> {
    let a = step(n)?;
    return Result::Ok(a as float);
}

fn code(Result<int, int> r) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(e) => 100 + e,
    };
}

test("returned try forwards the result") {
    assert(code(forward(4)) == 4)?;
    assert(code(forward(0)) == 99)?;
    assert(code(rewrap(5)) == 5)?;
    assert(code(rewrap(0)) == 99)?;
    assert((some_again(3) ?? 0) == 6)?;
    assert((some_again(0) ?? 7) == 7)?;
}

test("try miss returns the error pair") {
    assert(code(chain(3)) == 5)?;
    assert(code(chain(1)) == 99)?;
    assert(code(chain(0)) == 99)?;
    let w = match widen(0) {
        Result::Ok(_) => 0,
        Result::Err(e) => e,
    };
    assert(w == -1)?;
}
