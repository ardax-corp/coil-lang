// COI-287 W1: float +/−/÷ loop (no *) must run on dense MIR opcodes.
fn hot(float a, float b, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        s = s + xf / a - b;
        i = i + 1;
    }
    return s;
}

test("dense addf n4") {
    // i=0..3: -1 + -0.5 + 0 + 0.5 = -1.0
    assert(hot(2.0, 1.0, 4) == -1.0)?;
}

test("dense addf n3") {
    // i=0..2: -1 + -0.5 + 0 = -1.5
    assert(hot(2.0, 1.0, 3) == -1.5)?;
}
