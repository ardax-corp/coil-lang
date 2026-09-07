// COI-283: `(i as float) * 7.0` → add induction on dense SSA.
fn hot(int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = (i as float) * 7.0;
        s = s + xf * xf;
        i = i + 1;
    }
    return s;
}

test("iv sr cast times const") {
    // sum_{i=0..3} (7i)^2 = 49 * (0+1+4+9) = 686
    assert(hot(4) == 686.0)?;
}
