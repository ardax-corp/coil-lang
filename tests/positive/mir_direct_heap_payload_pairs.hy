// Two-word pairs whose payload may be a heap word (`Result<int, E>` with a
// heap `E`, a one-field enum with a class variant) in bodies lowered
// straight from HIR to MIR: the payload is an opaque word that only moves,
// and arms that bind it as a heap value stay with HIR emission.
class Fault {
    pub code: int,
}

enum Shape {
    Square(int),
    Labeled(Fault),
    Empty,
}

fn check(int x) -> Result<int, Fault> {
    if x % 7 == 6 {
        return Result::Err(new Fault(x));
    }
    return Result::Ok(x * 2);
}

fn total(int n) -> Result<int, Fault> {
    let t = 0;
    let i = 0;
    while i < n {
        t = t + check(i)?;
        i = i + 1;
    }
    return Result::Ok(t);
}

fn pass(int n) -> Result<int, Fault> {
    return total(n);
}

fn shape(int k) -> Shape {
    if k == 0 {
        return Shape::Empty;
    }
    if k < 0 {
        return Shape::Labeled(new Fault(k));
    }
    return Shape::Square(k);
}

fn area(int k) -> int {
    return match shape(k) {
        Shape::Square(s) => s * s,
        Shape::Labeled(f) => f.code,
        Shape::Empty => 0,
    };
}

fn ok_or(int n, int fallback) -> int {
    return match pass(n) {
        Result::Ok(v) => v,
        Result::Err(_) => fallback,
    };
}

test("heap payload pairs in direct MIR") {
    assert(ok_or(6, -1) == 30)?;
    assert(ok_or(7, -1) == -1)?;
    match pass(20) {
        Result::Ok(_) => assert(false)?,
        Result::Err(f) => assert(f.code == 6)?,
    }
    assert(area(5) == 25)?;
    assert(area(-3) == -3)?;
    assert(area(0) == 0)?;
}
