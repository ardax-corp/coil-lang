// `new C(args).field` in direct MIR: the object is never observed, so it
// is not built. Every argument still runs, in order, and the field's is
// the value.
use tests::assert;

class Point {
    pub x: int,
    pub y: int,
}

class Named {
    pub name: string,
    pub score: float,
}

static let trace: int = 0;

fn tick(int d) -> int {
    trace = trace * 10 + d;
    return d;
}

fn first(int a, int b) -> int {
    return new Point(a, b).x;
}

fn second(int a, int b) -> int {
    return (new Point(a, b)).y;
}

fn ordered() -> int {
    return new Point(tick(1), tick(2)).x;
}

test("field of a temporary object") {
    assert!(first(5, 6) == 5);
    assert!(second(5, 6) == 6);
    assert!(new Named("ab", 1.5).score == 1.5);
    assert!(new Named("ab", 1.5).name == "ab");
    assert!(ordered() == 1);
    assert!(trace == 12);
    assert!(new Point(tick(3), tick(4)).y == 4);
    assert!(trace == 1234);
}
