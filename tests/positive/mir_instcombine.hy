// COI-281: `t * 2.0` → add on dense SSA (plus leftover identities).
fn hot(float scale, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = (i as float) * scale;
        let t = xf * xf;
        s = ((s + t * 2.0) * 1.0) + 0.0;
        i = (i + 1) + 0;
    }
    return s;
}

test("instcombine mul2 sum") {
    assert(hot(2.0, 4) == 112.0)?;
}
