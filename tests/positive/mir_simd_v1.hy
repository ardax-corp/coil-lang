// COI-311: V1 reduce + conservative FMA must match scalar IEEE / wrapping math.

fn scan(Vec<int> v) -> int {
    let acc = 0;
    let i = 0;
    while i < len(v) {
        acc = acc + v[i];
        i = i + 1;
    }
    return acc;
}

fn fscan(Vec<float> v) -> float {
    let acc = 0.0;
    let i = 0;
    while i < len(v) {
        acc = acc + v[i];
        i = i + 1;
    }
    return acc;
}

fn axpy(float a, Vec<float> x, Vec<float> y) -> float {
    let i = 0;
    while i < len(x) {
        y[i] = a * x[i] + y[i];
        i = i + 1;
    }
    return y[0];
}

test("int scan 0..15 is 120") {
    let v: Vec<int> = Vec::from([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
    assert(scan(v) == 120)?;
}

test("float scan left-fold") {
    let v: Vec<float> = Vec::from([1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
    assert(fscan(v) == 45.0)?;
}

test("axpy store mul-then-add") {
    let x: Vec<float> = Vec::from([1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
    let y: Vec<float> = Vec::from([10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0]);
    let z = axpy(2.0, x, y);
    assert(z == 12.0)?;
    assert(y[7] == 26.0)?;
}
