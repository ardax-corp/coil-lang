// A repeated pure expression reads the local that holds its first value,
// but only while nothing it reads has changed.
class Box {
    pub v: int,
}

fn reuse(int a, int b) -> int {
    let x = a * b;
    return x + (a * b) + (b * a);
}

fn after_write(int a) -> int {
    let x = a + 1;
    a = 10;
    return x + (a + 1);
}

fn elements([int] xs, int k) -> int {
    let x = xs[k];
    xs[k] = 100;
    return x + xs[k];
}

fn fields(Box b) -> int {
    let x = b.v;
    b.v = b.v + 1;
    return x + b.v;
}

fn branches(int a, bool c) -> int {
    let x = a * 3;
    if c {
        a = 0;
        return x + a * 3;
    }
    return x + a * 3;
}

fn holder_changes(int a) -> int {
    let x = a * 2;
    x = 0;
    return x + a * 2;
}

test("same expression, operands either way round") {
    assert(reuse(2, 3) == 18)?;
}

test("a write to an operand ends the reuse") {
    assert(after_write(1) == 13)?;
}

test("element and field writes end the reuse") {
    assert(elements([1, 2, 3], 1) == 102)?;
    assert(fields(new Box(5)) == 11)?;
}

test("a branch that writes an operand") {
    assert(branches(2, true) == 6)?;
    assert(branches(2, false) == 12)?;
}

test("the holding local changes") {
    assert(holder_changes(4) == 8)?;
}
