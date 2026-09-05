// Lite IV strength reduction: i*c and stride loops stay numerically exact.

test("iv times const sums") {
    let n = 10;
    let s = 0;
    let i = 0;
    while i < n {
        s = s + i * 3;
        i = i + 1;
    }
    assert(s == 135)?;
}

test("iv times invariant slot") {
    let n = 8;
    let c = 7;
    let s = 0;
    let i = 0;
    while i < n {
        s = s + i * c;
        i = i + 1;
    }
    assert(s == 196)?;
}

test("stride iv times const") {
    let n = 20;
    let p = 3;
    let s = 0;
    let k = 2;
    while k < n {
        s = s + k * 2;
        k = k + p;
    }
    assert(s == 114)?;
}

test("cast iv is not rewritten") {
    let n = 5;
    let s = 0.0;
    let i = 0;
    while i < n {
        s = s + (i as float);
        i = i + 1;
    }
    assert(s == 10.0)?;
}
