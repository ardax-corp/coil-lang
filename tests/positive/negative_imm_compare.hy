// A compare-and-branch or op-and-store fused against a small negative
// literal (`n >= -32`, `m = n + -3`) keeps the literal's sign.

fn bucket(int n) -> int {
    if n >= 0 {
        if n <= 127 {
            return 1;
        }
        return 2;
    }
    if n >= -32 {
        return 3;
    }
    if n > -128 {
        return 4;
    }
    return 5;
}

fn below(int n) -> bool {
    if n < -1 {
        return true;
    }
    return false;
}

fn shift(int n) -> int {
    let m = n + -3;
    let k = m * -2;
    return k;
}

test("negative immediates keep their sign") {
    assert(bucket(5) == 1)?;
    assert(bucket(500) == 2)?;
    assert(bucket(-5) == 3)?;
    assert(bucket(-32) == 3)?;
    assert(bucket(-100) == 4)?;
    assert(bucket(-1000) == 5)?;
    assert(below(-2))?;
    assert(!below(-1))?;
    assert(!below(3))?;
    assert(shift(10) == -14)?;
    assert(shift(-1) == 8)?;
}
