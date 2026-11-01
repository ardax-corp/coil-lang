use tests::assert;

fn ok_ninety_nine() -> Result<int, string> {
    return Result::Ok(99);
}

fn err_boom() -> Result<int, string> {
    return Result::Err("boom");
}

test("explicit Result::Ok return in result-mode fn") {
    let r = ok_ninety_nine();
    let n = match r {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
    assert!(n == 99);
}

test("explicit Result::Err return in result-mode fn") {
    let r = err_boom();
    let ok = match r {
        Result::Ok(_) => false,
        Result::Err(e) => e == "boom",
    };
    assert!(ok);
}
