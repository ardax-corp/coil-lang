// An expected type applies to the expression it is for, not to its
// operands: a `return` / `==` context must not reach a match scrutinee or a
// comparison's operands. Value-forwarding nodes (groups, match arms, `??`,
// arithmetic) still carry it.

class Val {
    pub i: int,
}

class Point {
    pub x: int,
}

enum DecErr {
    Bad,
}

trait FromVal<T> {
    fn hydrate(T proto, Val v) -> Result<T, DecErr> {}
}

trait Size<T> {
    fn size(T self_, int k) -> int {}
}

impl FromVal for Point {
    pub fn hydrate(Point proto, Val v) -> Result<Point, DecErr> {
        return Result::Ok(new Point(v.i));
    }
}

impl Size for Point {
    pub fn size(Point self_, int k) -> int {
        return self_.x + k;
    }
}

fn scrutinee() -> int {
    return match new Point(0).hydrate(new Val(9)) {
        Result::Ok(p) => p.x,
        Result::Err(_) => -1,
    };
}

fn comparison_operand() -> bool {
    return new Point(new Point(2).size(3)).x == 5;
}

fn byte_match(int k) -> byte {
    let b: byte = match k {
        0 => 1,
        default => (2 + 3),
    };
    return b;
}

fn byte_coalesce(Option<byte> o) -> byte {
    let b: byte = o ?? 7;
    return b;
}

fn byte_arith() -> byte {
    return 1 + 1;
}

test("return match on a trait call") {
    assert(scrutinee() == 9)?;
}

test("trait call inside a comparison operand") {
    assert(comparison_operand())?;
}

test("expected byte still reaches arithmetic literals") {
    assert(byte_arith() == 2 as byte)?;
}

test("expected byte reaches match arms and groups") {
    assert(byte_match(0) == 1 as byte)?;
    assert(byte_match(4) == 5 as byte)?;
}

test("expected byte reaches the ?? fallback") {
    assert(byte_coalesce(Option::None) == 7 as byte)?;
    assert(byte_coalesce(Option::Some(3 as byte)) == 3 as byte)?;
}
