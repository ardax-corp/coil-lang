// A power-of-two divisor held in a local is also read elsewhere: `x / s`
// must stay a division (or multiply by `1/s`), never multiply by `s`.

fn ratio() -> float {
    let x = 6.0;
    let s = 8.0;
    return x / s;
}

fn ratio_and_sum(float x) -> float {
    let s = 8.0;
    return x / s + s;
}

test("division by a power-of-two local") {
    assert(ratio() == 0.75)?;
}

test("power-of-two divisor also added") {
    assert(ratio_and_sum(6.0) == 8.75)?;
}
