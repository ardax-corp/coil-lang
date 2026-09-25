// Integer identities: multiply/and-by-zero produce 0; or/xor/shift-by-zero
// and mul/div-by-one keep the value.
test("int mul and bitand zero") {
    let n = 41;
    assert((n * 0) == 0)?;
    assert((n & 0) == 0)?;
    assert((0 & n) == 0)?;
}

test("int or xor shift and unit factors") {
    let n = 41;
    assert((n | 0) == n)?;
    assert((n ^ 0) == n)?;
    assert((n << 0) == n)?;
    assert((n * 1) == n)?;
    assert((n / 1) == n)?;
    assert((n + 0) == n)?;
    assert((n - 0) == n)?;
}
