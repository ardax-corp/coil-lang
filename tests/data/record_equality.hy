// `==` / `!=` on records is structural, as on tuples: same fields, each
// compared as `==` compares it (#1618). Strings compare by content, nested
// records by their fields, and records inside arrays, tuples and Options
// compare the same way. Two names for one record are still equal.
use tests::assert;

fn build(string a, string b) -> string {
    return a + b;
}

fn id<T>(T x) -> T {
    return x;
}

enum Holder<T> {
    Empty,
    Held(T),
}

test("records compare by their fields") {
    let a = { a: 1 };
    let b = { a: 1 };
    assert!(a == b);
    assert!(!(a != b));
    assert!({ a: 1 } != { a: 2 });
    assert!(!({ a: 1 } == { a: 2 }));
    assert!(id({ x: 1, y: 2 }) == { x: 1, y: 2 });
    assert!(id({ x: 1, y: 2 }) != { x: 2, y: 1 });
    let c = a;
    assert!(a == c);
}

test("field order in the literal does not matter") {
    assert!({ x: 1, y: "a" } == { y: "a", x: 1 });
    assert!({ x: 1, y: "a" } != { y: "b", x: 1 });
}

test("string fields compare by content") {
    let s = build("ab", "c");
    assert!({ n: 1, s: s } == { n: 1, s: "abc" });
    assert!({ n: 1, s: s } != { n: 1, s: "abd" });
}

test("float and bool fields") {
    assert!({ f: 1.5, ok: true } == { f: 1.5, ok: true });
    assert!({ f: 1.5, ok: true } != { f: 1.5, ok: false });
    assert!({ f: 1.5, ok: true } != { f: 2.5, ok: true });
}

test("nested records") {
    assert!({ p: { q: 2 } } == { p: { q: 2 } });
    assert!({ p: { q: 2 } } != { p: { q: 3 } });
    assert!({ p: { q: build("x", "") }, n: 1 } == { p: { q: "x" }, n: 1 });
}

test("records inside arrays and Vecs") {
    assert!([{ a: 1 }, { a: 2 }] == [{ a: 1 }, { a: 2 }]);
    assert!([{ a: 1 }, { a: 2 }] != [{ a: 1 }, { a: 3 }]);
    let v = Vec::new();
    v.push({ a: 1 });
    let w = Vec::new();
    w.push({ a: 1 });
    assert!(v == w);
    w.push({ a: 2 });
    assert!(v != w);
}

test("records inside tuples") {
    assert!(({ a: 1 }, 2) == ({ a: 1 }, 2));
    assert!(({ a: 1 }, 2) != ({ a: 2 }, 2));
    assert!((1, { s: build("a", "b") }) == (1, { s: "ab" }));
}

test("records inside Options and enums") {
    assert!(Some({ a: 1 }) == Some({ a: 1 }));
    assert!(Some({ a: 1 }) != Some({ a: 2 }));
    assert!(Some({ a: 1 }) != None);
    assert!(Holder::Held({ n: 1, s: build("h", "i") }) == Holder::Held({ n: 1, s: "hi" }));
    assert!(Holder::Held({ n: 1, s: "hi" }) != Holder::Held({ n: 2, s: "hi" }));
    assert!(Holder::Held({ n: 1, s: "hi" }) != Holder::Empty);
}
