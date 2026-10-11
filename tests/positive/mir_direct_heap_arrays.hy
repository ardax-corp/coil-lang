// Heap arrays and `Vec`s of `int` / `float` in bodies lowered straight from
// HIR to MIR: parameters and results, `for x in xs` (with `continue`),
// index reads (with the Euclidean fix-up for `i % m`) and writes, and
// `len`.

fn total(Vec<int> v) -> int {
    let t = 0;
    for x in v {
        t = t + x * 3;
    }
    return t;
}

fn odd_sum([int] xs) -> int {
    let t = 0;
    for x in xs {
        if x % 2 == 0 {
            continue;
        }
        t = t + x;
    }
    return t;
}

fn scale(Vec<float> xs, float k) {
    let i = 0;
    while i < len(xs) {
        xs[i] = xs[i] * k + 1.0;
        i = i + 1;
    }
}

fn ring([int] xs, int k) -> int {
    return xs[k % 3] * 10 + xs[(k - 7) % 3];
}

fn bump([int] xs, int at, int by) -> [int] {
    xs[at] = xs[at] + by;
    return xs;
}

fn mean(Vec<float> v) -> float {
    let s = 0.0;
    for x in v {
        s = s + x;
    }
    return s / (len(v) as float);
}

test("heap arrays and Vecs in direct MIR") {
    let v: Vec<int> = Vec::new();
    let i = 0;
    while i < 20 {
        v.push(i);
        i = i + 1;
    }
    assert(total(v) == 570)?;
    assert(odd_sum([1, 2, 3, 4, 5, 6, 7]) == 16)?;
    let f: Vec<float> = Vec::new();
    f.push(1.0);
    f.push(2.5);
    scale(f, 2.0);
    assert(f[0] == 3.0)?;
    assert(f[1] == 6.0)?;
    assert(mean(f) == 4.5)?;
    assert(ring([4, 5, 6], 4) == 54)?;
    assert(ring([4, 5, 6], 1) == 54)?;
    let b = bump([1, 2, 3], 2, 40);
    assert(b[2] == 43)?;
}
