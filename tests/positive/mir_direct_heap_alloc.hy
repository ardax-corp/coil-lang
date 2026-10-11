// Heap array allocation in bodies lowered straight from HIR to MIR: array
// literals, and stack arrays boxed where they escape (every later read and
// write goes through the box, with its bounds check).

fn sum([int] xs) -> int {
    let t = 0;
    for x in xs {
        t = t + x;
    }
    return t;
}

fn rows(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        let row = [i, i * 2, i * 3];
        acc = acc + sum(row) + row[1];
        i = i + 1;
    }
    return acc;
}

fn fresh(float k) -> [float] {
    let a = [k, k * 2.0, k * 3.0];
    a[0] = a[0] + 0.5;
    return a;
}

fn escape_then_write(int k) -> int {
    let a = [1, 2, 3, 4];
    let before = sum(a);
    a[k % 4] = a[k % 4] * 10;
    return before * 1000 + sum(a) + a[3];
}

test("heap array allocation in direct MIR") {
    assert(rows(10) == 360)?;
    let f = fresh(1.5);
    assert(f[0] == 2.0)?;
    assert(f[2] == 4.5)?;
    assert(escape_then_write(1) == 10032)?;
    assert(escape_then_write(-1) == 10086)?;
}
