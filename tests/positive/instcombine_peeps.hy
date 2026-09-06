// InstCombine (#304): const-cond, XOR-1 identity, two-slot payload match.

fn id_result(int n) -> Result<int, int> {
    if n < 0 {
        return Result::Err(0 - n);
    }
    return Result::Ok(n);
}

fn xor_twice(int x) -> int {
    return (x ^ 1) ^ 1;
}

test("const false branch is skipped") {
    let x = 0;
    if false {
        x = 1;
    }
    assert(x == 0)?;
}

test("const true branch is taken") {
    let x = 0;
    if true {
        x = 2;
    }
    assert(x == 2)?;
}

test("xor one twice is identity") {
    assert(xor_twice(42) == 42)?;
    assert(xor_twice(0) == 0)?;
    assert(xor_twice(1) == 1)?;
}

test("identity result match keeps payload") {
    assert(match id_result(7) {
        Result::Ok(v) => v,
        Result::Err(e) => e,
    } == 7)?;
    assert(match id_result(-3) {
        Result::Ok(v) => v,
        Result::Err(e) => e,
    } == 3)?;
}
