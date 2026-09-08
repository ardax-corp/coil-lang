// COI-291: typed dense→dense CALL (leaf kernel + looping caller).
fn kernel(float x) -> float {
    let a = x * x + x * 2.0;
    let b = a * x + x * 4.0;
    let c = b * x - a * 0.5;
    return c / (2.0 + x);
}

fn hot(float a, float dx, int n) -> float {
    let i = 0;
    let s = 0.0;
    let x = 0.125;
    while i < n {
        s = s + kernel(x) * a + dx;
        x = x + dx;
        i = i + 1;
    }
    return s;
}

test("dense call n1") {
    assert(hot(1.0, 0.0, 1) == kernel(0.125))?;
}

test("dense call n2") {
    assert(hot(2.0, 0.0, 2) == kernel(0.125) * 4.0)?;
}
