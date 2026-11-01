// Building a `Range<T>` / `RangeInclusive<T>` in a generic `T: Ord` body:
// returned, bound to a local, forwarded, and through a function value
// (#1230).
use tests::assert;

fn span<T: Ord>(T a, T b) -> Range<T> {
    return a..b;
}

fn upto<T: Ord>(T a, T b) -> RangeInclusive<T> {
    let r = a..=b;
    return r;
}

fn twice<T: Ord>(T a, T b) -> Range<T> {
    return span(a, b);
}

test("a generic range over int") {
    let r = span(0, 5);
    assert!((typeof r) == "prelude::Range<int>");
    assert!(r.to_vec().len() == 5);
    let total = 0;
    for i in span(1, 4) {
        total += i;
    }
    assert!(total == 6);
}

test("a generic inclusive range bound to a local") {
    let v = upto(1, 3).to_vec();
    assert!(v.len() == 3);
    assert!(v[2] == 3);
}

test("a generic range over float") {
    let f = span(1.0, 3.0).to_vec();
    assert!(f.len() == 2);
    assert!(f[1] == 2.0);
}

test("a generic range forwarded and through a function value") {
    assert!(twice(0, 3).to_vec().len() == 3);
    let f: (int, int) -> Range<int> = span;
    assert!(f(2, 6).to_vec().len() == 4);
}
