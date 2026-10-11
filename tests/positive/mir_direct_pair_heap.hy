// Heap payloads in two-word results, and enum values kept boxed in one
// local, in bodies lowered straight from HIR to MIR.
class Fault {
    pub code: int,
}

enum Shape {
    Dot,
    Square(int),
}

fn div(int a, int b) -> Result<int, Fault> {
    if b == 0 {
        return Result::Err(new Fault(a));
    }
    return Result::Ok(a / b);
}

fn twice(int a, int b) -> Result<int, Fault> {
    let q = div(a, b)?;
    return Result::Ok(q * 2);
}

fn code_of(Result<int, Fault> r) -> int {
    match r {
        Result::Ok(n) => {
            return n;
        },
        Result::Err(f) => {
            return -f.code;
        },
    }
}

fn side(Shape s) -> int {
    match s {
        Shape::Square(n) => {
            return n;
        },
        Shape::Dot => {
            return 0;
        },
    }
}

fn apply(Shape -> int f, Shape s) -> int {
    return f(s);
}

test("heap payloads in pairs in direct MIR") {
    assert(code_of(twice(9, 3)) == 6)?;
    assert(code_of(twice(7, 0)) == -7)?;
    assert(apply(side, Shape::Square(4)) == 4)?;
    assert(apply(side, Shape::Dot) == 0)?;
}
