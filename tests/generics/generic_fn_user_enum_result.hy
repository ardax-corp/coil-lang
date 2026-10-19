// A generic function returning a user generic enum (`Slot<T>`) called
// directly at ground types: the mono clone's result is read in the enum's
// own layout (#1567).
use tests::assert;

enum Slot<T> {
    Full(T),
    Empty,
}

enum Either<A, B> {
    L(A),
    R(B),
}

class P {
    pub n: int,
}

fn slot<T>(T x, bool full) -> Slot<T> {
    if full {
        return Slot::Full(x);
    }
    return Slot::Empty;
}

fn either<A, B>(A a, B b, bool left) -> Either<A, B> {
    if left {
        return Either::L(a);
    }
    return Either::R(b);
}

test("int payload") {
    let n = match slot(5, true) {
        Slot::Full(v) => v,
        Slot::Empty => -1,
    };
    assert!(n == 5);
    let e = match slot(5, false) {
        Slot::Full(v) => v,
        Slot::Empty => -1,
    };
    assert!(e == -1);
}

test("class payload") {
    let n = match slot(new P(7), true) {
        Slot::Full(p) => p.n,
        Slot::Empty => -1,
    };
    assert!(n == 7);
    let e = match slot(new P(7), false) {
        Slot::Full(p) => p.n,
        Slot::Empty => -1,
    };
    assert!(e == -1);
}

test("float and string payloads") {
    let f = match slot(2.5, true) {
        Slot::Full(v) => v,
        Slot::Empty => 0.0,
    };
    assert!(f == 2.5);
    let s = match slot("hi", true) {
        Slot::Full(v) => v,
        Slot::Empty => "",
    };
    assert!(s == "hi");
}

test("two type parameters") {
    let l = match either(3, "x", true) {
        Either::L(n) => n,
        Either::R(_) => -1,
    };
    assert!(l == 3);
    let r = match either(3, "x", false) {
        Either::L(_) => "",
        Either::R(s) => s,
    };
    assert!(r == "x");
}
