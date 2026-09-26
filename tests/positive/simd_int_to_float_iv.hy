// Vectorized `i as float` on the IV must convert `i` before the f64 splat,
// not reuse its integer bits.
fn scaled_sum(float scale, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        s = s + xf * scale / 8.0;
        i = i + 1;
    }
    return s;
}

test("int-to-float IV in a vector reduction") {
    assert(scaled_sum(1.5, 2000) == 374812.5)?;
}
