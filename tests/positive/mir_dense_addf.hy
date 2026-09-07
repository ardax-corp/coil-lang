// COI-287 W1: float +/− loop (no * or /) must run on dense MIR opcodes.
fn hot(float a, float b, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        s = s + xf + a - b;
        i = i + 1;
    }
    return s;
}

test("dense addf n4") {
    // i=0..3: (i+2-1) = 1+2+3+4 = 10
    assert(hot(2.0, 1.0, 4) == 10.0)?;
}

test("dense addf n3") {
    // i=0..2: 1+2+3 = 6
    assert(hot(2.0, 1.0, 3) == 6.0)?;
}
