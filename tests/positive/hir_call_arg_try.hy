// A `?` or `??` in a call argument after the first lowers from HIR: every
// argument runs into a temp at depth zero, then they reload in order.
fn mul(int a, int b) -> Result<int, string> {
    if a > 1000 || b > 1000 {
        return Result::Err("overflow");
    }
    return Result::Ok(a * b);
}

fn add(int a, int b) -> Result<int, string> {
    return Result::Ok(a + b);
}

fn pick(int n) -> Option<int> {
    if n > 0 {
        return Option::Some(n);
    }
    return Option::None;
}

fn sub3(int a, int b, int c) -> int {
    return a - b - c;
}

fn total(int x, int y) -> Result<int, string> {
    let acc = mul(x, 2)?;
    acc = add(acc, mul(y, 3)?)?;
    acc = add(acc, add(1, mul(x, y)?)?)?;
    return Result::Ok(acc);
}

fn ordered(int n) -> int {
    return sub3(100, pick(n) ?? 7, n);
}

test("try in a later call argument") {
    assert((total(2, 3) ?? -1) == 4 + 9 + 7)?;
    assert((total(2, 2000) ?? -1) == -1)?;
    assert((total(1001, 1) ?? -1) == -1)?;
}

test("coalesce in a later call argument keeps argument order") {
    assert(ordered(5) == 90)?;
    assert(ordered(0) == 93)?;
}
