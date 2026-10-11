// `Vec::push` in bodies lowered straight from HIR to MIR: the Vec, then
// the value, then the push (a safepoint that may grow the storage); reads
// after it see the new length and element.

fn fill(int n) -> Vec<int> {
    let v: Vec<int> = Vec::with_capacity(1);
    let i = 0;
    while i < n {
        v.push(i * i);
        i = i + 1;
    }
    return v;
}

fn grow(Vec<float> v, int k) -> float {
    let i = 0;
    while i < k {
        v.push(v[len(v) - 1] * 0.5);
        i = i + 1;
    }
    return v[len(v) - 1];
}

fn both(Vec<int> a, Vec<int> b) -> int {
    let i = 0;
    while i < 5 {
        a.push(len(b));
        b.push(a[i] + 1);
        i = i + 1;
    }
    return len(a) * 100 + b[len(b) - 1];
}

test("Vec push in direct MIR") {
    let v = fill(1000);
    assert(len(v) == 1000)?;
    assert(v[999] == 998001)?;
    let f: Vec<float> = Vec::new();
    f.push(64.0);
    assert(grow(f, 6) == 1.0)?;
    assert(len(f) == 7)?;
    let a: Vec<int> = Vec::new();
    let b: Vec<int> = Vec::new();
    b.push(7);
    assert(both(a, b) == 506)?;
}
