// COI-291: typed dense→dense CALL (leaf kernel + looping caller).
fn kernel(float x, int k) -> float {
    let i = 0;
    let t = x;
    while i < k {
        t = t * t + x;
        i = i + 1;
    }
    return t;
}

fn hot(float a, float dx, int n) -> float {
    let i = 0;
    let s = 0.0;
    let x = 0.125;
    while i < n {
        s = s + kernel(x, 8) * a + dx;
        x = x + dx;
        i = i + 1;
    }
    return s;
}

test("dense call n1") {
    assert(hot(1.0, 0.0, 1) == kernel(0.125, 8))?;
}

test("dense call n2") {
    assert(hot(2.0, 0.0, 2) == kernel(0.125, 8) * 4.0)?;
}
