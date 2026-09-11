// Counted for/range IPA (COI-362 E4). Trip count > COIL_PAR_THRESHOLD.
// Checksums must match sequential AUTO_PAR=0.

fn sq(int i) -> int {
    return i * i;
}

test("literal for-range reduce") {
    let acc = 0;
    for x in 0..40 {
        acc = acc + sq(x);
    }
    assert(acc == 20540)?;
}

test("inclusive for-range reduce") {
    let acc = 0;
    for x in 1..=40 {
        acc += x;
    }
    assert(acc == 820)?;
}

test("const range local reduce") {
    let r = 0..40;
    let acc = 0;
    for x in r {
        acc = acc + x;
    }
    assert(acc == 780)?;
}
