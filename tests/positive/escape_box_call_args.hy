// A local that escapes as a call arg is boxed at the statement start; the
// arg spill that parks args above that box must not overwrite live args.
class Cell {
    pub a: int,
    pub b: int,
}

fn sum_n(Cell c, int n) -> int {
    let s = 0;
    let i = 0;
    while i < n {
        s = s + c.a + c.b;
        i = i + 1;
    }
    return s;
}

test("escaped class arg next to a literal arg") {
    let c = new Cell(1, 2);
    let r = sum_n(c, 10);
    assert(r == 30)?;
    assert(c.a == 1)?;
}

test("two escaping stack arrays in one host call") {
    let a = [[1, 2], [3, 4]];
    let b = [[5, 6], [7, 8]];
    let m = matmul(a, b);
    assert(m[0][0] == 19)?;
    assert(m[1][1] == 50)?;
}
