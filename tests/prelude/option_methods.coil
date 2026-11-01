// The core prelude's `Option` methods, on immediate (two-slot), niche
// (`Option<string>`) and object payloads, and through a shared generic body.
use tests::assert;

class Point {
    pub x: int,
}

fn double(int x) -> int {
    return x * 2;
}

fn half(int x) -> Option<int> {
    if x % 2 == 0 {
        return Some(x / 2);
    }
    return None;
}

// Called at an open `T`: the methods run their shared bodies.
fn lift<T>(Option<T> o, T fallback) -> T {
    return o.or(Some(fallback)).unwrap_or(fallback);
}

fn mapped<T>(Option<T> o, T -> T f) -> Option<T> {
    return o.map(f).and_then(fn (T v) => Some(v));
}

test("is_some and is_none") {
    let a: Option<int> = Some(3);
    let b: Option<string> = None;
    assert!(a.is_some() && !a.is_none());
    assert!(b.is_none() && !b.is_some());
}

test("unwrap_or and unwrap_or_else") {
    let a: Option<int> = Some(3);
    let n: Option<int> = None;
    assert!(a.unwrap_or(9) == 3);
    assert!(n.unwrap_or(9) == 9);
    let k = 40;
    assert!(n.unwrap_or_else(fn () use (k) => k + 2) == 42);
    let s: Option<string> = None;
    assert!(s.unwrap_or("none") == "none");
}

test("map and map_or") {
    let a: Option<int> = Some(4);
    let n: Option<int> = None;
    assert!(a.map(double) == Some(8));
    assert!(n.map(double) == None);
    assert!(a.map(fn (int x) => "x" + x.show()) == Some("x4"));
    assert!(a.map_or(0, double) == 8);
    assert!(n.map_or(-1, double) == -1);
}

test("and_then chains") {
    let a: Option<int> = Some(8);
    assert!(a.and_then(half).and_then(half) == Some(2));
    assert!(a.and_then(half).and_then(half).and_then(half).and_then(half) == None);
    let s: Option<string> = Some("hi");
    assert!(s.and_then(fn (string t) => Some(t + "!")) == Some("hi!"));
}

test("or, or_else and filter") {
    let a: Option<int> = Some(1);
    let n: Option<int> = None;
    assert!(a.or(Some(2)) == Some(1));
    assert!(n.or(Some(2)) == Some(2));
    assert!(n.or_else(fn () => Some(5)) == Some(5));
    assert!(a.filter(fn (int x) => x > 0) == Some(1));
    assert!(a.filter(fn (int x) => x > 5) == None);
}

test("ok_or") {
    let a: Option<int> = Some(1);
    let n: Option<int> = None;
    assert!(a.ok_or("missing") == Ok(1));
    assert!(n.ok_or("missing") == Err("missing"));
}

test("object payloads") {
    let p: Option<Point> = Some(new Point(3));
    let q: Option<Point> = None;
    assert!(p.map(fn (Point v) => v.x) == Some(3));
    assert!(q.map(fn (Point v) => v.x) == None);
    assert!(p.is_some() && q.is_none());
}

test("shared generic bodies") {
    assert!(lift(Some(1), 2) == 1);
    assert!(lift(None, "b") == "b");
    assert!(mapped(Some(3), double) == Some(6));
    assert!(mapped(Some("a"), fn (string s) => s + s) == Some("aa"));
}
