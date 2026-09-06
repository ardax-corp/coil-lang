// LICM + integer SR (#315): nested invariant chains and i*c stay exact.

fn nested_chains(int n) -> int {
    let s = 0;
    let y = 0;
    while y < n {
        let x = 0;
        while x < n {
            s = s + (n * 3 + 1);
            s = s + (n * 5 + 2);
            x = x + 1;
        }
        y = y + 1;
    }
    return s;
}

fn iv_mul(int n, int c) -> int {
    let s = 0;
    let i = 0;
    while i < n {
        s = s + i * c;
        i = i + 1;
    }
    return s;
}

test("nested invariant chains") {
    // Each inner step adds (n*3+1) + (n*5+2) = 8n+3, n^2 times.
    assert(nested_chains(3) == 243)?;
    assert(nested_chains(1) == 11)?;
    assert(nested_chains(0) == 0)?;
}

test("integer iv times const") {
    assert(iv_mul(10, 3) == 135)?;
    assert(iv_mul(1, 9) == 0)?;
    assert(iv_mul(0, 4) == 0)?;
}
