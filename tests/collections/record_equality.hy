// `==` on records compares their fields (#1618), whatever order the
// literals name them in, also inside arrays, tuples, enums and other
// records. Class instances still compare by identity.
use tests::assert;

class P {
    pub x: int,
}

test("equal records") {
    assert!({ a: 1 } == { a: 1 });
    let x = { a: 1, s: "q" };
    let y = { s: "q", a: 1 };
    assert!(x == y);
    assert!(!(x != y));
}

test("different records") {
    assert!({ a: 1 } != { a: 2 });
    assert!({ a: 1, s: "q" } != { a: 1, s: "r" });
}

test("nested") {
    assert!({ p: { a: 1 } } == { p: { a: 1 } });
    assert!({ p: { a: 1 } } != { p: { a: 2 } });
    assert!([{ a: 1 }] == [{ a: 1 }]);
    assert!([{ a: 1 }] != [{ a: 2 }]);
    assert!((1, { a: 1 }) == (1, { a: 1 }));
    assert!(Some({ a: 1 }) == Some({ a: 1 }));
    assert!(Some({ a: 1 }) != Some({ a: 2 }));
}

test("float fields compare as floats") {
    assert!({ f: 0.0 } == { f: -0.0 });
    assert!([{ f: 1.5 }] == [{ f: 1.5 }]);
}

test("class instances compare by identity") {
    let a = new P(1);
    let b = new P(1);
    assert!(a != b);
    assert!({ p: a } != { p: b });
    assert!({ p: a } == { p: a });
}
