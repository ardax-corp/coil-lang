// Direct CALL/RETURN two-slot ABI: immediate match uses payload+tag slots.

fn maybe(int n) -> Option<int> {
    if n < 0 {
        return Option::None;
    }
    return Option::Some(n);
}

fn div(int a, int b) -> Result<int, int> {
    if b == 0 {
        return Result::Err(-1);
    }
    return Result::Ok(a / b);
}

fn parse(int n, int fail) -> Result<int, string> {
    if fail == 1 {
        return Result::Err("bad");
    }
    return Result::Ok(n);
}

enum Cell {
    Num(int),
    Empty,
}

fn cell(int n) -> Cell {
    if n < 0 {
        return Cell::Empty;
    }
    return Cell::Num(n);
}

fn niche_text() -> Option<string> {
    return Option::Some("ok");
}

test("option int direct match") {
    assert(match maybe(3) {
        Option::Some(v) => v == 3,
        Option::None => false,
    })?;
    assert(match maybe(-1) {
        Option::Some(_) => false,
        Option::None => true,
    })?;
}

test("result int int direct match") {
    assert(match div(10, 2) {
        Result::Ok(q) => q == 5,
        Result::Err(_) => false,
    })?;
    assert(match div(1, 0) {
        Result::Ok(_) => false,
        Result::Err(e) => e == -1,
    })?;
}

test("result int heap error") {
    assert(match parse(9, 0) {
        Result::Ok(v) => v == 9,
        Result::Err(_) => false,
    })?;
    assert(match parse(9, 1) {
        Result::Ok(_) => false,
        Result::Err(_) => true,
    })?;
}

test("payload enum arity 1") {
    assert(match cell(4) {
        Cell::Num(v) => v == 4,
        Cell::Empty => false,
    })?;
    assert(match cell(-2) {
        Cell::Num(_) => false,
        Cell::Empty => true,
    })?;
}

test("niched option stays one word") {
    assert(match niche_text() {
        Option::Some(s) => s == "ok",
        Option::None => false,
    })?;
}

test("bind site still matches") {
    let x = maybe(2);
    assert(match x {
        Option::Some(v) => v == 2,
        Option::None => false,
    })?;
}
