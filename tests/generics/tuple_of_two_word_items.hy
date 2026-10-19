// A generic returning a tuple of its type parameter, called at a two-word
// type (`Option<int>`, `Result<int, int>`, a range): the result is a heap
// tuple whose items ride boxed, and destructuring, indexing or looping over
// it unboxes each item (#1486).
use tests::assert;

fn both<T>(T a, T b) -> (T, T) {
    return (a, b);
}

fn three<T>(T a, T b, T c) -> (T, T, T) {
    return (a, b, c);
}

fn mixed<T>(T a, int n) -> (T, int) {
    return (a, n);
}

fn nest<T>(T a, T b) -> (int, (T, T)) {
    return (1, (a, b));
}

fn pairs<T>(T a, T b) -> [(T, T); 2] {
    return [(a, b), (b, a)];
}

fn vec_of<T>(T a, T b) -> Vec<(T, T)> {
    return [(a, b)];
}

test("a tuple of Option<int> from a generic destructures") {
    let x: Option<int> = Option::Some(5);
    let y: Option<int> = Option::None;
    let (a, b) = both(x, y);
    assert!(a == Option::Some(5));
    assert!(b == Option::None);
}

test("a tuple of Result<int, int> from a generic destructures") {
    let x: Result<int, int> = Result::Ok(5);
    let y: Result<int, int> = Result::Err(7);
    let (a, b) = both(x, y);
    assert!(a == Result::Ok(5));
    assert!(b == Result::Err(7));
}

test("a tuple of ranges from a generic destructures") {
    let (a, b) = both(1..3, 4..6);
    let s = 0;
    for i in a {
        s += i;
    }
    for i in b {
        s += i * 10;
    }
    assert!(s == 93);
}

test("a non-generic tuple of Option<int> destructures") {
    let x: Option<int> = Option::Some(5);
    let y: Option<int> = Option::None;
    let t = (x, y);
    let (a, b) = t;
    assert!(a == Option::Some(5));
    assert!(b == Option::None);
}

test("a tuple of Option<int> indexes") {
    let x: Option<int> = Option::Some(5);
    let y: Option<int> = Option::None;
    let t = both(x, y);
    assert!(t[0] == Option::Some(5));
    assert!(t[1] == Option::None);
}

test("three items, mixed items and nested tuples destructure") {
    let x: Option<int> = Option::Some(5);
    let y: Option<int> = Option::None;
    let (a, b, c) = three(x, y, x);
    assert!(a == Option::Some(5));
    assert!(b == Option::None);
    assert!(c == Option::Some(5));
    let (d, n) = mixed(y, 3);
    assert!(d == Option::None);
    assert!(n == 3);
    let (m, (e, f)) = nest(x, y);
    assert!(m == 1);
    assert!(e == Option::Some(5));
    assert!(f == Option::None);
}

test("tuples of Option<int> in an array and a Vec") {
    let x: Option<int> = Option::Some(5);
    let y: Option<int> = Option::None;
    let xs = pairs(x, y);
    let (a, b) = xs[1];
    assert!(a == Option::None);
    assert!(b == Option::Some(5));
    let k = 0;
    for (p, q) in xs {
        assert!(p != q);
        k += 1;
    }
    assert!(k == 2);
    let vs = vec_of(x, y);
    let (c, d) = vs[0];
    assert!(c == Option::Some(5));
    assert!(d == Option::None);
    for (p, q) in vs {
        assert!(p == Option::Some(5));
        assert!(q == Option::None);
    }
}
