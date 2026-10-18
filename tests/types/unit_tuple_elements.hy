// A tuple may hold `()` elements: bound, destructured, passed and stored
// in a `Vec` (#1427).
use tests::assert;

fn snd((unit, int) p) -> int {
    let (_, n) = p;
    return n;
}

fn mark() {}

test("a local tuple with a unit element destructures") {
    let pair = ((), 1);
    let (_, n) = pair;
    assert!(n == 1);
    assert!(pair[1] == 1);
}

test("a tuple with a unit element is passed") {
    assert!(snd(((), 4)) == 4);
    assert!(snd((mark(), 5)) == 5);
}

test("a vec of tuples with unit elements") {
    let v: Vec<((), int)> = [((), 1), ((), 2)];
    assert!(len(v) == 2);
    let (u, n) = v[1];
    assert!(n == 2);
    assert!(u == ());
}
