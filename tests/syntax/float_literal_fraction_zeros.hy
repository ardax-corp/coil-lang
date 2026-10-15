// #997: a float literal whose fraction starts with zeros (`0.05`, `1.03`)
// parses, and keeps every digit of the fraction.
use tests::assert;

static let RATE: float = 0.05;
const STEP = 1.003;

test("leading zeros in the fraction") {
    let a = 0.05;
    let b = 1.03;
    let c = 2.0001;
    assert!(a * 100.0 > 4.99);
    assert!(a * 100.0 < 5.01);
    assert!(b - 1.0 > 0.0299);
    assert!(b - 1.0 < 0.0301);
    assert!(c > 2.0);
    assert!(c < 2.001);
}

test("a fraction of only zeros") {
    let z = 3.000;
    assert!(z == 3.0);
    let w = 0.0;
    assert!(w == 0.0);
}

test("statics, consts and negated literals") {
    assert!(RATE + RATE == 0.1);
    assert!(STEP > 1.0);
    assert!(STEP < 1.01);
    let n = -0.05;
    assert!(n + 0.05 == 0.0);
}

test("same value as the plain spelling") {
    assert!(0.50 == 0.5);
    assert!(10.010 == 10.01);
}
