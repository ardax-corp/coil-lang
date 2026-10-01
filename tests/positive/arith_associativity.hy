// #578: `*` `/` `%` chains associate left; `**` is right-associative and
// binds tighter than them.

fn mixed(int a, int b, int c) -> int {
    return a * b / c;
}

test("multiplicative operators associate left") {
    let a = 100;
    let b = 7;
    let c = 3;
    assert(a / b / c == 4, "100 / 7 / 3")?;
    assert(a / b * c == 42, "100 / 7 * 3")?;
    assert(a * b / c == 233, "100 * 7 / 3")?;
    assert(a % b * c == 6, "100 % 7 * 3")?;
    assert(a * b % c == 1, "100 * 7 % 3")?;
    assert(mixed(5, 1000, 7) == 714, "runtime operands")?;
}

test("constant folding keeps the grouping") {
    let t = 5 * 1000 / 7;
    assert(t == 714, "5 * 1000 / 7 folds to 714")?;
    assert(100 / 7 * 3 == 42, "100 / 7 * 3 folds to 42")?;
}

test("power is right-associative and tighter than factor") {
    assert(2 ** 3 ** 2 == 512, "2 ** (3 ** 2)")?;
    assert(2 ** 3 * 4 == 32, "(2 ** 3) * 4")?;
    assert(2 * 3 ** 2 == 18, "2 * (3 ** 2)")?;
    assert(64 / 2 ** 3 == 8, "64 / (2 ** 3)")?;
}

test("float chains associate left") {
    let x = 3.0;
    let s = 8.0;
    assert(2.0 * x / s == 0.75, "(2.0 * 3.0) / 8.0")?;
}
