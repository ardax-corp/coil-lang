// #845: a literal inside a non-literal cast operand is not pinned to byte.

fn shift_up(int n) -> byte {
    return (32 + n) as byte;
}

fn letter(int k) -> byte {
    let c = (65 + k) as byte;
    return c;
}

test("int expression with a literal casts to byte") {
    assert(shift_up(3) == 35 as byte)?;
    assert(letter(1) == "B" as byte)?;
    assert(((70 + 3) as byte) == 73 as byte)?;
}
