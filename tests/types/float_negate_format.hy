// Unary `-` on a float literal and on a float parameter is float negation
// (not an int negate of the bits), as printed by `%f`.
use tests::assert;
use string::format;

fn negf(float x) -> float {
    return -x;
}

test("float negation prints as the negated value") {
    let out = format("%f,%f", -0.55, negf(0.55));
    assert!(out == "-0.55,-0.55", out);
}
