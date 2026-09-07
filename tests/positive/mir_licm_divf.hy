// COI-280: invariant DIVF in a dense float-mul loop.
fn hot(float a, float b, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        let q = a / b;
        s = s + q * xf;
        i = i + 1;
    }
    return s;
}

test("licm invariant divf sum") {
    assert(hot(3.0, 2.0, 8) == 42.0)?;
}
