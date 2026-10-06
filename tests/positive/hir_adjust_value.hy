// `x++` / `--x` used as values lower from HIR: one INC / DEC on the slot.
fn bump(int n) -> int {
    let k = n;
    let a = k++;
    let b = ++k;
    return a * 100 + b;
}

test("adjust values inside expressions") {
    assert(bump(3) == 305)?;
    let i = 10;
    let s = i-- + --i;
    assert(s == 18)?;
    assert(i == 8)?;
}
