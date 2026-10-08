// A callee whose guard clauses return early inlines as one `if` value:
// `if c { return a; } rest` becomes `if c { a } else { rest }`.

fn clamp(int x, int lo, int hi) -> int {
    if x < lo {
        return lo;
    }
    if x > hi {
        return hi;
    }
    return x;
}

fn sign(int x) -> int {
    if x < 0 {
        return -1;
    } else if x == 0 {
        return 0;
    } else {
        return 1;
    }
}

fn step(int x) -> int {
    let y = x * 2;
    if y > 10 {
        let z = y - 10;
        return z * 3;
    }
    return y + 1;
}

fn note(int x) {
    if x < 0 {
        return;
    }
    return;
}

test("guard returns pick the right exit") {
    assert(clamp(-5, 0, 9) == 0, "below")?;
    assert(clamp(5, 0, 9) == 5, "inside")?;
    assert(clamp(50, 0, 9) == 9, "above")?;
}

test("an if chain whose branches all return") {
    assert(sign(-3) == -1, "negative")?;
    assert(sign(0) == 0, "zero")?;
    assert(sign(8) == 1, "positive")?;
}

test("a guard with statements before and inside it") {
    assert(step(3) == 7, "small")?;
    assert(step(8) == 18, "large")?;
}

fn mix(int n) -> int {
    let acc = 0;
    let i = 0 - n;
    while i < n {
        note(i);
        acc = acc + clamp(i, -5, 5) + sign(i) + step(i);
        i = i + 1;
    }
    return acc;
}

test("guards in a loop and a unit callee") {
    assert(mix(20) == 260, "sum")?;
}
