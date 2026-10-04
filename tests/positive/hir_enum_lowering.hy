// Bodies the HIR lowering covers since enums, Option/Result and `match`:
// boxed and two-slot dispatch, in-place payload bindings, `?` / `??`,
// Result-mode returns and payloads built above live operands. The language
// harness also runs under `--hir`, so each case pins both codegens.
enum Shape {
    Dot,
    Circle(int),
    Rect(int, int),
}

fn id(int x) -> int {
    return x;
}

fn area(Shape s) -> int {
    return match s {
        Shape::Dot => 0,
        Shape::Circle(r) => r * r * 3,
        Shape::Rect(w, h) => w * h,
    };
}

fn half(int x) -> Option<int> {
    if x % 2 == 0 {
        return Option::Some(x / 2);
    }
    return Option::None;
}

fn quarter(int x) -> Option<int> {
    let h = half(x)?;
    return half(h);
}

fn orz(int x) -> int {
    return quarter(x) ?? -1;
}

fn safe_div(int a, int b) -> Result<int, string> {
    if b == 0 {
        raise "div by zero";
    }
    return a / b;
}

fn calc(int a, int b) -> Result<int, string> {
    let q = safe_div(a, b)?;
    return q + 1;
}

fn checked_div(int a, int b) -> Result<int, int> {
    if b == 0 {
        return Result::Err(-1);
    }
    return Result::Ok(a / b);
}

fn ok_or_zero(Result<int, int> r) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => 0,
    };
}

fn err_scaled(int a, int b) -> int {
    let r = checked_div(a, b);
    return match r {
        Result::Err(e) => e * 100,
        Result::Ok(v) => v,
    };
}

fn bind_then_try(int a, int b) -> Result<int, int> {
    let r = checked_div(a, b);
    let q = r?;
    return Result::Ok(q);
}

fn sum_after(int x) -> int {
    return x * 1000 + area(Shape::Rect(id(x + 1), id(x + 2))) + x;
}

fn step(Shape s) -> int {
    match s {
        Shape::Dot => {
            return -1;
        },
        Shape::Circle(_) => {
            return 1;
        },
        Shape::Rect(_, _) => {
            return 2;
        },
    }
}

test("boxed match binds multi-field payloads in place") {
    assert(area(Shape::Rect(3, 4)) == 12)?;
    assert(area(Shape::Circle(2)) == 12)?;
    assert(area(Shape::Dot) == 0)?;
}

test("try and coalesce on two-slot Options") {
    assert(orz(8) == 2)?;
    assert(orz(6) == -1)?;
    assert(orz(5) == -1)?;
}

test("Result-mode returns and raise") {
    assert(match calc(10, 2) {
        Result::Ok(n) => n == 6,
        Result::Err(_) => false,
    })?;
    assert(match calc(1, 0) {
        Result::Ok(_) => false,
        Result::Err(e) => e == "div by zero",
    })?;
}

test("identity arms and bound locals of two-slot calls") {
    assert(ok_or_zero(checked_div(8, 2)) == 4)?;
    assert(ok_or_zero(checked_div(8, 0)) == 0)?;
    assert(err_scaled(8, 2) == 4)?;
    assert(err_scaled(8, 0) == -100)?;
    assert(match bind_then_try(8, 2) {
        Result::Ok(v) => v == 4,
        Result::Err(_) => false,
    })?;
}

test("payload built above a live operand") {
    assert(sum_after(1) == 1007)?;
}

test("statement match whose arms all return") {
    assert(step(Shape::Dot) == -1)?;
    assert(step(Shape::Circle(3)) == 1)?;
    assert(step(Shape::Rect(1, 2)) == 2)?;
}
