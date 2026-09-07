// COI-270: two-slot Result<int,int> leaf must stay off MakeEnum / dense.
fn checked_div(int a, int b) -> Result<int, int> {
    if b == 0 {
        return Result::Err(-1);
    }
    return Result::Ok(a / b);
}

test("ok divides") {
    assert(match checked_div(6, 3) {
        Result::Ok(q) => q == 2,
        Result::Err(_) => false,
    })?;
}

test("err on zero") {
    assert(match checked_div(1, 0) {
        Result::Ok(_) => false,
        Result::Err(e) => e == -1,
    })?;
}
