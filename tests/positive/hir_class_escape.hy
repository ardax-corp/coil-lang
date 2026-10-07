// A class local whose fields are written before it escapes keeps them in
// frame slots and is boxed once, at the statement (or block tail) where it
// first escapes, as the AST's unboxed class local.
class Pair {
    pub a: int,
    pub b: int,
}

fn sum(Pair p) -> int {
    return p.a + p.b;
}

fn bump(Pair p) {
    p.a = p.a + 100;
}

fn chain(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        let p = new Pair(i, 0);
        p.b = i * 2;
        acc = acc + sum(p);
        i = i + 1;
    }
    return acc;
}

fn shared() -> int {
    let p = new Pair(1, 2);
    p.a = 10;
    bump(p);
    p.b = p.b + 5;
    return p.a + p.b;
}

// The last statement of a test body is its block tail.
test("fields written before an escape in the tail") {
    let p = new Pair(1, 2);
    p.a = 5;
    assert(sum(p) == 7)?;
}

test("writes after the escape go through the box") {
    assert(shared() == 117)?;
}

test("escape in a loop") {
    assert(chain(4) == 18)?;
}
