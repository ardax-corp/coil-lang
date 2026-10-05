// An int-backed scalar enum with no operator instance is its backing word:
// arithmetic is the raw int opcodes, as in the AST.
#[repr(int)]
enum Code {
    Low = 10,
    High = 250,
}

fn offset(Code c, int by) -> int {
    return c + by;
}

fn spread() -> int {
    return Code::High - Code::Low;
}

fn scaled(Code c) -> int {
    return c * 2 / 5;
}

test("scalar enum arithmetic") {
    assert(offset(Code::Low, 5) == 15)?;
    assert(spread() == 240)?;
    assert(scaled(Code::High) == 100)?;
}
