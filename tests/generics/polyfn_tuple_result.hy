// A call through a `PolyFn` value may return a tuple: a two-word pair
// or a heap tuple (#1511).
use tests::assert;
use string::format;

fn id<T>(T x) -> T {
    return x;
}

fn mk<T>(T x) -> (int, float) {
    return (3, 1.5);
}

fn first<T>(T x, int n) -> T {
    return x;
}

test("a polyfn returns a pair argument") {
    let i = id;
    let (a, b) = i((1, 2));
    assert!(a == 1);
    assert!(b == 2);
    let p = i((2.5, true));
    assert!(format("%v", p) == "(2.5, true)");
}

test("a polyfn returns a concrete pair") {
    let m = mk;
    let (g, h) = m("z");
    assert!(g == 3);
    assert!(h == 1.5);
    let fi = first;
    let (j, k) = fi((9, 10), 0);
    assert!(j + k == 19);
}

test("a polyfn returns a heap tuple") {
    let i = id;
    let q = i(("s", 4));
    assert!(format("%v", q) == "(s, 4)");
    let (a, (b, c)) = i((1, (2, 3)));
    assert!(a + b + c == 6);
}
