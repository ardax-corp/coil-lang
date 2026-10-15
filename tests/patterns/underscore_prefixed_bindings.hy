// A name that starts with `_` (`_b`, `_rx`) is an ordinary binding in every
// pattern position, not a `_` wildcard followed by junk (#1072).
use tests::assert;

fn pair() -> (int, int) {
    return (1, 2);
}

test("let tuple pattern binds an underscore-prefixed name") {
    let (a, _b) = pair();
    assert!(a == 1);
    assert!(_b == 2);
}

test("let tuple pattern with only underscore-prefixed names") {
    let (_x, _y) = pair();
    assert!(_x + _y == 3);
}

test("nested tuple pattern") {
    let (_outer, (_i, j)) = (5, (6, 7));
    assert!(_outer + _i + j == 18);
}

test("bare underscore is still a wildcard") {
    let (_, b) = pair();
    assert!(b == 2);
}

test("for-in binds an underscore-prefixed name") {
    let sum = 0;
    for _v in [1, 2, 3] {
        sum += _v;
    }
    assert!(sum == 6);
}

test("for-in tuple pattern binds an underscore-prefixed name") {
    let sum = 0;
    for (_a, b) in [(1, 10), (2, 20)] {
        sum += _a + b;
    }
    assert!(sum == 33);
}

test("match payload binds an underscore-prefixed name") {
    let o = Option::Some(4);
    let v = match o {
        Option::Some(_n) => _n,
        Option::None => 0,
    };
    assert!(v == 4);
}
