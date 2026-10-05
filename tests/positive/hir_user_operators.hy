// Operators over user types lower from HIR as the AST codegen emits them:
// a call of the type's trait instance (derived or written), or `EQ` / `NEQ`
// when the type has no `Eq` instance.

class Vec2 {
    pub x: int,
    pub y: int,
}

impl Add for Vec2 {
    pub fn add(Vec2 a, Vec2 b) -> Vec2 {
        return new Vec2(a.x + b.x, a.y + b.y);
    }
}

impl Sub for Vec2 {
    pub fn sub(Vec2 a, Vec2 b) -> Vec2 {
        return new Vec2(a.x - b.x, a.y - b.y);
    }
}

enum Color {
    Red,
    Blue,
}

#[derive(Eq, Ord)]
enum Rank {
    Low,
    Mid,
    High,
}

fn same(Color a, Color b) -> bool {
    return a == b;
}

fn differ(Color a, Color b) -> bool {
    return a != b;
}

fn below(Rank a, Rank b) -> bool {
    return a < b;
}

fn at_most(Rank a, Rank b) -> bool {
    return a <= b;
}

fn above(Rank a, Rank b) -> bool {
    return a > b;
}

fn at_least(Rank a, Rank b) -> bool {
    return a >= b;
}

fn rank_eq(Rank a, Rank b) -> bool {
    return a == b;
}

fn opt_is(Option<int> o) -> bool {
    return o == Option::Some(3);
}

fn span(Vec2 a, Vec2 b) -> int {
    let d = b - a;
    return d.x + d.y;
}

fn shift(Vec2 a) -> Vec2 {
    return a + new Vec2(1, 1);
}

test("enum equality without an Eq instance") {
    assert(same(Color::Red, Color::Red))?;
    assert(!same(Color::Red, Color::Blue))?;
    assert(differ(Color::Red, Color::Blue))?;
    assert(!differ(Color::Blue, Color::Blue))?;
}

test("derived Eq and Ord") {
    assert(below(Rank::Low, Rank::High))?;
    assert(!below(Rank::High, Rank::Mid))?;
    assert(at_most(Rank::Mid, Rank::Mid))?;
    assert(above(Rank::High, Rank::Low))?;
    assert(at_least(Rank::Low, Rank::Low))?;
    assert(!at_least(Rank::Low, Rank::Mid))?;
    assert(rank_eq(Rank::Mid, Rank::Mid))?;
    assert(!rank_eq(Rank::Mid, Rank::High))?;
}

test("option equality") {
    assert(opt_is(Option::Some(3)))?;
    assert(!opt_is(Option::Some(4)))?;
    assert(!opt_is(Option::None))?;
}

test("class operators") {
    assert(span(new Vec2(1, 2), new Vec2(4, 8)) == 9)?;
    let s = shift(new Vec2(2, 3));
    assert(s.x == 3 && s.y == 4)?;
}
