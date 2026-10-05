// HIR lowering of `panic`: as a statement, as a match arm value and with
// a built message. The panics never run; the bodies lower from HIR.

fn checked(int n) -> int {
    if n < 0 {
        panic "negative";
    }
    return n * 2;
}

fn unwrap_or_panic(Option<int> o) -> int {
    return match o {
        Option::Some(v) => v,
        Option::None => panic "none",
    };
}

fn named(int n) -> int {
    if n > 100 {
        let what = "too big";
        panic "named: " + what;
    }
    return n;
}

fn first_ok(Result<int, string> r) -> int {
    let v = match r {
        Result::Ok(v) => v,
        Result::Err(e) => panic e,
    };
    return v + 1;
}

test("panic paths not taken") {
    assert(checked(4) == 8)?;
    assert(unwrap_or_panic(Option::Some(7)) == 7)?;
    assert(named(5) == 5)?;
    assert(first_ok(Result::Ok(1)) == 2)?;
}
