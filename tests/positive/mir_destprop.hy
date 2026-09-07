// COI-282: same-value join after InstCombine; DestProp forwards t → a.
fn hot(float a, float b, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = (i as float) * b;
        let t = 0.0;
        if xf > a {
            t = a + 0.0;
        } else {
            t = a * 1.0;
        }
        s = s + a * xf + t * xf + t * t;
        i = i + 1;
    }
    return s;
}

test("destprop alias join") {
    assert(hot(1.0, 2.0, 4) == 28.0)?;
}
