// COI-286: counted saxpy-reduce must match sequential f64 math.
fn pack(float a, float x0, float dx, float y, int n) -> float {
    let i = 0;
    let s = 0.0;
    let x = x0;
    while i < n {
        s = s + a * x + y;
        x = x + dx;
        i = i + 1;
    }
    return s;
}

test("axpy pack sum 0..99 is 4950") {
    assert(pack(1.0, 0.0, 1.0, 0.0, 100) == 4950.0)?;
}

test("axpy pack a=2 x0=1 dx=1 y=0 n=4") {
    assert(pack(2.0, 1.0, 1.0, 0.0, 4) == 20.0)?;
}

test("axpy pack with y addend") {
    assert(pack(1.0, 0.0, 1.0, 1.0, 3) == 6.0)?;
}
