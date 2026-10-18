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

fn take(unit u) -> int {
    return 7;
}

fn mk(int n) -> ((), int, ()) {
    return ((), n, ());
}

test("a unit element is read, returned and iterated") {
    let pair = ((), 1);
    assert!(pair[0] == ());
    assert!(take(pair[0]) == 7);
    let (a, b, c) = mk(3);
    assert!(b == 3);
    assert!(a == c);
    let v: Vec<((), int)> = [((), 1), ((), 2)];
    let s = 0;
    for (u, n) in v {
        s = s + n;
    }
    assert!(s == 3);
}
