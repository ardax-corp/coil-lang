// `**` on floats lowers to direct MIR: variable operands, a fractional
// and a negative exponent, `**=` in a loop and a right-associated chain.
use tests::assert;

fn pow(float b, float e) -> float {
    return b ** e;
}

fn grow(float x, int n) -> float {
    let i = 0;
    while i < n {
        x **= 2.0;
        i = i + 1;
    }
    return x;
}

test("variable operands") {
    assert!(pow(2.0, 10.0) == 1024.0);
    assert!(pow(9.0, 0.5) == 3.0);
    assert!(pow(2.0, -2.0) == 0.25);
    assert!(pow(5.0, 0.0) == 1.0);
}

test("pow assign in a loop") {
    assert!(grow(2.0, 3) == 256.0);
    assert!(grow(1.5, 0) == 1.5);
}

test("right associative") {
    let a = pow(2.0, 1.0);
    let b = 3.0;
    assert!(a ** b ** 2.0 == 512.0);
    assert!((a ** b) ** 2.0 == 64.0);
}
