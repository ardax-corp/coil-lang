// A call through a `PolyFn` value may take and return a niche enum, an
// `Option` or a `Result`, as its bare type parameter (#1515).
use tests::assert;

fn id<T>(T x) -> T {
    return x;
}

enum Color {
    Red,
    Green,
    Blue,
}

enum Shape {
    Circle(float),
    Rect(int, int),
    Empty,
}

enum Wrap<T> {
    W(T),
    Nothing,
}

fn second<A, B>(A a, B b) -> B {
    return b;
}

fn mkopt<T>(T x) -> Option<int> {
    return Some(7);
}

fn mkres<T>(T x) -> Result<int, string> {
    return Err("no");
}

test("a polyfn returns an Option argument") {
    let i = id;
    let o = i(Some(3));
    let a = match o {
        Some(v) => v,
        None => -1,
    };
    assert!(a == 3);
    let n: Option<int> = None;
    let b = match i(n) {
        Some(v) => v,
        None => -1,
    };
    assert!(b == -1);
    let c = match i(Some("x")) {
        Some(s) => s,
        None => "",
    };
    assert!(c == "x");
}

test("a polyfn returns a Result argument") {
    let i = id;
    let r: Result<int, string> = Ok(4);
    let a = match i(r) {
        Ok(v) => v,
        Err(_) => -1,
    };
    assert!(a == 4);
    let e: Result<int, string> = Err("bad");
    let b = match i(e) {
        Ok(_) => "",
        Err(m) => m,
    };
    assert!(b == "bad");
}

test("a polyfn returns a concrete Option or Result") {
    let m = mkopt;
    let a = match m(1) {
        Some(v) => v,
        None => -1,
    };
    assert!(a == 7);
    let k = mkres;
    let b = match k("a") {
        Ok(_) => "",
        Err(m) => m,
    };
    assert!(b == "no");
}

test("a polyfn returns an Option of a pair") {
    let i = id;
    let o = i(Some((1, 2)));
    let t = match o {
        Some(t) => t,
        None => (0, 0),
    };
    let (a, b) = t;
    assert!(a + b == 3);
}

test("a polyfn returns a unit Result and a float or bool Option") {
    let i = id;
    let u: Result<(), string> = Err("u");
    let a = match i(u) {
        Ok(_) => "",
        Err(m) => m,
    };
    assert!(a == "u");
    let ok: Result<(), string> = Ok(());
    let b = match i(ok) {
        Ok(_) => 1,
        Err(_) => 0,
    };
    assert!(b == 1);
    let c = match i(Some(2.5)) {
        Some(x) => x,
        None => 0.0,
    };
    assert!(c == 2.5);
    let d = match i(Some(true)) {
        Some(x) => x,
        None => false,
    };
    assert!(d);
}

test("a polyfn returns a user enum") {
    let i = id;
    let n = match i(Color::Green) {
        Color::Red => 0,
        Color::Green => 1,
        Color::Blue => 2,
    };
    assert!(n == 1);
    let m = match i(Shape::Rect(2, 3)) {
        Shape::Circle(_) => 0,
        Shape::Rect(a, b) => a * b,
        Shape::Empty => -1,
    };
    assert!(m == 6);
    let f = match i(Shape::Circle(1.5)) {
        Shape::Circle(r) => r,
        default => 0.0,
    };
    assert!(f == 1.5);
    let s = match i(Wrap::W("hi")) {
        Wrap::W(x) => x,
        Wrap::Nothing => "",
    };
    assert!(s == "hi");
}

test("a polyfn takes an Option beside other arguments") {
    let i = id;
    let a = match i(Some(Some(4))) {
        Some(Some(x)) => x,
        default => -1,
    };
    assert!(a == 4);
    let s = second;
    let r: Result<int, string> = Ok(9);
    let b = match s(Some(1), r) {
        Ok(x) => x,
        Err(_) => -1,
    };
    assert!(b == 9);
    let c = match s((1, 2), None) {
        Some(x) => x,
        None => 5,
    };
    assert!(c == 5);
    let (d, e) = s(Some(1), (3, 4));
    assert!(d + e == 7);
}
