// Floats keep IEEE-754 behaviour: dividing by zero gives `inf` / `-inf`
// (no panic), `NaN` is unordered (every `<` `<=` `>` `>=` with it is
// false) and `sqrt` / `ln` produce NaN and `-inf` instead of clamping.
// (`==` on NaN and -0.0 is in float_eq_ieee.hy.)
use tests::assert;

fn fdiv(float a, float b) -> float {
    return a / b;
}

// NaN is the only value that is neither `<=` nor `>` itself.
fn is_nan(float x) -> bool {
    return !(x <= x) && !(x > x);
}

test("division by zero is infinite") {
    let inf = fdiv(1.0, 0.0);
    let ninf = fdiv(-1.0, 0.0);
    assert!(inf > 1000000000000.0);
    assert!(ninf < -1000000000000.0);
    assert!(inf == inf + 1.0);
    assert!(ninf == -inf);
    assert!(!is_nan(inf));
}

test("zero over zero is NaN") {
    let nan = fdiv(0.0, 0.0);
    assert!(is_nan(nan));
    assert!(!(nan < 1.0));
    assert!(!(nan > 1.0));
    assert!(!(nan >= -1.0));
    assert!(!(nan <= nan));
    assert!(!(nan >= nan));
    assert!(is_nan(nan + 1.0));
    assert!(is_nan(nan * 0.0));
}

test("infinity arithmetic") {
    let inf = fdiv(1.0, 0.0);
    assert!(is_nan(inf - inf));
    assert!(is_nan(inf * 0.0));
    assert!(fdiv(1.0, inf) == 0.0);
    assert!(inf * -1.0 == fdiv(-1.0, 0.0));
}

test("negative zero orders equal to zero but keeps its sign") {
    let nz = -0.0;
    assert!(nz <= 0.0 && nz >= 0.0);
    assert!(!(nz < 0.0));
    assert!(fdiv(1.0, nz) < 0.0);
    assert!(fdiv(1.0, 0.0) > 0.0);
}

test("float overflow gives infinity") {
    let big = 1000000000000000.0 * 1000000000000000.0;
    let p = big * big * big * big * big * big * big * big * big * big * big;
    assert!(p == fdiv(1.0, 0.0));
    assert!(-p == fdiv(-1.0, 0.0));
}

test("libm exceptional inputs") {
    assert!(is_nan(sqrt(-1.0)));
    assert!(is_nan(ln(-1.0)));
    assert!(ln(0.0) == fdiv(-1.0, 0.0));
    assert!(sqrt(fdiv(1.0, 0.0)) == fdiv(1.0, 0.0));
    assert!(exp(1000.0) == fdiv(1.0, 0.0));
    assert!(exp(-1000.0) == 0.0);
}

test("float remainder keeps the dividend sign") {
    assert!(5.5 % 2.0 == 1.5);
    assert!(-5.5 % 2.0 == -1.5);
    assert!(5.5 % -2.0 == 1.5);
    assert!(is_nan(fdiv(1.0, 1.0) % 0.0));
}
