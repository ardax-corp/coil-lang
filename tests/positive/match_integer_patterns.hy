// Integer literal arms compare the `int` scrutinee; `default` or a binding
// closes the match (an `int` match without one is E0209).
fn classify(int code) -> int {
    return match code {
        200 => 1,
        404 => 2,
        default => 0,
    };
}

fn describe(int n) -> int {
    match n {
        1 => {
            return 10;
        },
        2 => {
            return 30;
        },
        other => {
            return other * 100;
        },
    }
}

fn big(int n) -> int {
    return match n {
        4000000000 => 1,
        default => 0,
    };
}

test("value match on integer literals") {
    assert(classify(200) == 1)?;
    assert(classify(404) == 2)?;
    assert(classify(500) == 0)?;
}

test("statement match with returns and a binding catch-all") {
    assert(describe(1) == 10)?;
    assert(describe(2) == 30)?;
    assert(describe(7) == 700)?;
}

test("literal outside i32 uses the constant pool") {
    assert(big(4000000000) == 1)?;
    assert(big(4) == 0)?;
}
