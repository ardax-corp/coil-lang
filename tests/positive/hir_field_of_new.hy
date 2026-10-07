// `new C(args).f` runs every argument in order and yields the field's;
// the object is never observed, so it is not built.

class P {
    pub x: int,
    pub y: int,
}

class Q {
    pub p: P,
    pub n: int,
}

static let calls: int = 0;

fn tick(int v) -> int {
    calls = calls * 10 + v;
    return v;
}

test("field of new reads its argument") {
    assert(new P(2, 3).x == 2)?;
    assert(new P(4, 5).y + 1 == 6)?;
}

test("every argument runs in order") {
    calls = 0;
    let y = new P(tick(1), tick(2)).y;
    assert(y == 2)?;
    assert(calls == 12)?;
}

test("nested new") {
    assert(new Q(new P(7, 8), 1).p.y == 8)?;
}
