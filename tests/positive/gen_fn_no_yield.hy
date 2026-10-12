// A `gen fn` that never yields runs to its return on the first resume.
gen fn scaled(int x, int k) -> int {
    let n = 0;
    let i = 0;
    while i < k {
        n = n * 2 + x;
        i = i + 1;
    }
    return n;
}

gen fn half(float x) -> float {
    if x < 0.0 {
        return 0.0;
    }
    return x / 2.0;
}

test("an int gen fn with a dependent loop and no yield") {
    let g = scaled(7, 3);
    assert((resume g) == 49)?;
    let h = scaled(7, 0);
    assert((resume h) == 0)?;
}

test("a float gen fn with an early return and no yield") {
    let g = half(5.0);
    assert((resume g) == 2.5)?;
    let h = half(-1.0);
    assert((resume h) == 0.0)?;
}
