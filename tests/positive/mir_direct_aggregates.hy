// Frame-slot aggregates in bodies lowered straight from HIR to MIR: stack
// arrays read and written at literal and proven indices (`i % m`, with the
// Euclidean fix-up for a negative dividend), copied, and refilled, and
// class locals kept one slot per field.

class Vec2 {
    pub x: int,
    pub y: float,
}

fn rotate(int n) -> int {
    let acc = 0;
    let i = 0 - n;
    while i < n {
        let xs = [i, i * 2, i * 3];
        acc = acc + xs[i % 3];
        xs[(i + 1) % 3] = 7;
        acc = acc + xs[0] + xs[1] + xs[2];
        i = i + 1;
    }
    return acc;
}

fn copies(int n) -> int {
    let a = [1, 2, 3, 4];
    let acc = 0;
    for i in 0..n {
        let b = a;
        b[i % 4] = b[i % 4] + i;
        acc = acc + b[0] * 1000 + b[3];
        a = [a[1], a[2], a[3], a[0]];
    }
    return acc;
}

fn floats(int n) -> float {
    let ws = [0.5, 0.25];
    let acc = 0.0;
    for i in 0..n {
        acc = acc + ws[i % 2] * (i as float);
    }
    return acc;
}

fn walk(int n) -> float {
    let v = new Vec2(0, 1.0);
    for i in 0..n {
        v.x = v.x + i;
        v.y = v.y * 2.0;
    }
    return (v.x as float) + v.y;
}

test("stack arrays from direct MIR") {
    assert(rotate(5) == 36)?;
    assert(copies(6) == 17018)?;
    assert(floats(5) == 4.0)?;
}

test("class fields from direct MIR") {
    assert(walk(4) == 22.0)?;
}
