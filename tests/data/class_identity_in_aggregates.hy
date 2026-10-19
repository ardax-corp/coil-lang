// A class value inside an array, tuple or Option compares by identity,
// with or without an `Eq` instance: only records compare by their fields
// (#1618).
use tests::assert;

class P {
    pub x: int,
}

#[derive(Eq)]
class Q {
    pub x: int,
}

test("class instances in arrays compare by identity") {
    let a = new P(1);
    let b = new P(1);
    assert!([a] == [a]);
    assert!([a] != [b]);
    assert!((a, 1) != (b, 1));
    assert!(Some(a) != Some(b));
    assert!(Some(a) == Some(a));
}

test("a derived Eq is not used inside arrays") {
    let a = new Q(1);
    let b = new Q(1);
    assert!(a == b);
    assert!([a] == [a]);
    assert!([a] != [b]);
}

test("a record holding a class compares the class by identity") {
    let a = new P(1);
    let b = new P(1);
    assert!({ p: a, n: 1 } == { p: a, n: 1 });
    assert!({ p: a, n: 1 } != { p: b, n: 1 });
}
