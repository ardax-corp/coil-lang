// A two-word `[payload, tag]` value whose variants carry a `float` and an
// `int` keeps both payloads in one word, the float as its bits. Direct MIR
// moves the float through that word and back unchanged: made, returned,
// forwarded by `?`, matched and bound.
use tests::assert;

enum Num {
    I(int),
    F(float),
}

fn step(int n) -> Result<int, int> {
    if n == 0 {
        return Result::Err(-1);
    }
    return Result::Ok(n);
}

fn widen(int n) -> Result<float, int> {
    let a = step(n)?;
    return Result::Ok(a as float + 0.5);
}

fn again(int n) -> Result<float, int> {
    let x = widen(n)?;
    return Result::Ok(x * 2.0);
}

fn inverse(float x) -> Result<int, float> {
    if x < 0.0 {
        return Result::Err(x);
    }
    return Result::Ok(1);
}

fn num(int n) -> Num {
    if n < 0 {
        return Num::F(n as float / 4.0);
    }
    return Num::I(n);
}

fn as_float(Num v) -> float {
    return match v {
        Num::I(i) => i as float,
        Num::F(f) => f,
    };
}

test("a float payload beside an int one") {
    let w = match widen(3) {
        Result::Ok(f) => f,
        Result::Err(_) => 0.0,
    };
    assert!(w == 3.5);
    let e = match widen(0) {
        Result::Ok(_) => 0,
        Result::Err(e) => e,
    };
    assert!(e == -1);
    let a = match again(1) {
        Result::Ok(f) => f,
        Result::Err(_) => 0.0,
    };
    assert!(a == 3.0);
    let bad = match inverse(-2.25) {
        Result::Ok(_) => 0.0,
        Result::Err(x) => x,
    };
    assert!(bad == -2.25);
    assert!(as_float(num(-2)) == -0.5);
    assert!(as_float(num(6)) == 6.0);
}
