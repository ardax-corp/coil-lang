// Ground operands of arithmetic / ordering operators: numeric types, string
// `+`, and types with an instance of the operator's trait stay accepted
// while non-numeric operands are a type error (`tests/compile_fail/arith_*`,
// `compare_bool_ordering`) (#554).

class Vec2 {
    pub x: int,
    pub y: int,
}

impl Sub for Vec2 {
    pub fn sub(Vec2 a, Vec2 b) -> Vec2 {
        return new Vec2(a.x - b.x, a.y - b.y);
    }
}

#[derive(Eq, Ord)]
enum Level {
    Low,
    High,
}

test("numeric operands, including byte, and string +") {
    let b: byte = 200 as byte;
    assert(b / (16 as byte) == 12 as byte)?;
    assert(b % (16 as byte) == 8 as byte)?;
    assert(7 % 3 == 1)?;
    assert(2 ** 10 == 1024)?;
    assert(2.5 * 2.0 == 5.0)?;
    assert((6 & 3) == 2)?;
    assert((1 << 4) == 16)?;
    assert("a" + "b" == "ab")?;
}

test("operator on a type with the trait instance") {
    let d = new Vec2(5, 7) - new Vec2(1, 2);
    assert(d.x == 4)?;
    assert(d.y == 5)?;
}

test("ordering through a derived Ord instance and on numerics") {
    assert(Level::Low < Level::High)?;
    assert(1.5 <= 2.0)?;
    assert((3 as byte) > (2 as byte))?;
}
