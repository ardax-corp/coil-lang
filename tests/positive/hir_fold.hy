// Operators on literals fold, identities keep their operand, and an `if`
// on a literal keeps the branch it takes. Nothing that has an effect or
// does not fit the inline constant range is folded away.
static let calls: int = 0;

fn bump(int v) -> int {
    calls = calls + 1;
    return v;
}

fn constants() -> int {
    return (3 + 4) * 2 - 10 / 3 + 17 % 5 + (1 << 4) + (256 >> 2) + (6 & 3) + (6 | 1) + (6 ^ 3);
}

fn identities(int x) -> int {
    return (x + 0) + (0 + x) + (x - 0) + (x * 1) + (1 * x) + (x / 1) + (x | 0) + (x ^ 0) + (x << 0) +
           (x & -1);
}

fn zero_with_effect() -> int {
    return bump(5) * 0;
}

fn wide() -> int {
    return 2147483647 + 1;
}

fn wide_product() -> int {
    return 4000000 * 4000000;
}

fn floats(float x) -> float {
    return x * 1.0 + 1.5 * 2.0 + x / 1.0;
}

fn picks(bool b) -> int {
    let r = 0;
    if true {
        r = r + 1;
    } else {
        r = r + 100;
    }
    if false {
        r = r + 1000;
    }
    if !!b && true {
        r = r + 10;
    }
    if false || !false {
        r = r + 20;
    }
    return r - -5;
}

test("operators on literals") {
    assert(constants() == 107)?;
}

test("identities keep the operand") {
    assert(identities(7) == 70)?;
    assert(identities(-3) == -30)?;
}

test("a call times zero still runs") {
    calls = 0;
    assert(zero_with_effect() == 0)?;
    assert(calls == 1)?;
}

test("results past 32 bits are computed at run time") {
    assert(wide() == 2147483648)?;
    assert(wide_product() == 16000000000000)?;
}

test("float folds") {
    assert(floats(2.0) == 7.0)?;
}

test("an if on a literal keeps the branch it takes") {
    assert(picks(true) == 36)?;
    assert(picks(false) == 26)?;
}
