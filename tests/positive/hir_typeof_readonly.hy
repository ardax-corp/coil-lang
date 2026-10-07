// `readonly e` is `e`'s value, `typeof e` its type's name (unevaluated),
// and `len` of a literal its item count (unevaluated).
use string::format;

static let hits: int = 0;

class Point {
    pub x: int,
    pub y: int,
}

fn bump() -> int {
    hits = hits + 1;
    return hits;
}

test("readonly values read through") {
    let xs = readonly [4, 5, 6];
    assert(len(xs) == 3)?;
    let p = readonly new Point(1, 2);
    assert(p.x + p.y == 3)?;
}

test("typeof names the static type") {
    let a = typeof 42;
    let b = typeof "hi";
    let c = typeof new Point(0, 0);
    assert(a == "int")?;
    assert(b == "string")?;
    assert(format("%s", typeof (1, 2)) == "(int, int)")?;
    assert(len(c) > 0)?;
}

test("typeof and literal len do not evaluate") {
    hits = 0;
    let t = typeof bump();
    assert(t == "int")?;
    assert(len([bump(), bump()]) == 2)?;
    assert(len((bump(), 1, 2)) == 3)?;
    assert(len({ a: bump(), b: 2 }) == 2)?;
    assert(hits == 0)?;
}
