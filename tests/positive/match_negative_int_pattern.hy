// #897: a negative int literal is a match pattern, like a positive one.
fn sign(int n) -> int {
    return match n {
        -1 => 1,
        0 => 2,
        -9223372036854775808 => 4,
        other => 3,
    };
}

test("negative int literals match as patterns") {
    assert(sign(-1) == 1)?;
    assert(sign(0) == 2)?;
    assert(sign(1) == 3)?;
    assert(sign(-2) == 3)?;
    assert(sign(-9223372036854775807 - 1) == 4)?;
}
