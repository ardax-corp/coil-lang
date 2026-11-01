// `Option` and `Result` over shown types have a `Show` instance: `Some(1)`,
// `None`, `Ok(2)`, `Err(bad)`, the payload as `%v` shows it. `show(o)`,
// `o.show()` and `format("%v", o)` agree, called directly, through a `Show`
// bound (a monomorphic clone) and through a shared generic body's
// dictionary (a `PolyFn` value), and inside tuples and arrays (#1479).
use tests::assert;
use string::format;

class Point {
    pub x: int,
    pub y: int,
}

impl Show for Point {
    pub fn show(Point p) -> string {
        return format("<%i %i>", p.x, p.y);
    }
}

fn shown<T: Show>(T x) -> string {
    return show(x);
}

fn shown_method<T: Show>(T x) -> string {
    return x.show();
}

fn find(int n) -> Option<int> {
    if n > 0 {
        return Some(n);
    }
    return None;
}

fn half(int n) -> Result<int, string> {
    if n % 2 != 0 {
        raise "odd";
    }
    return n / 2;
}

test("show of Option") {
    let o: Option<int> = Some(1);
    let n: Option<int> = None;
    assert!(show(o) == "Some(1)", show(o));
    assert!(show(n) == "None", show(n));
    assert!(o.show() == "Some(1)", o.show());
    assert!(show(find(3)) == "Some(3)", show(find(3)));
    assert!(show(find(0)) == "None", show(find(0)));
    let s: Option<string> = Some("s");
    assert!(show(s) == "Some(s)", show(s));
    let u: Option<()> = Some(());
    assert!(show(u) == "Some(())", show(u));
    let p: Option<Point> = Some(new Point(1, 2));
    assert!(show(p) == "Some(<1 2>)", show(p));
    let t: Option<(int, int)> = Some((1, 2));
    assert!(show(t) == "Some((1, 2))", show(t));
    let nested: Option<Option<int>> = Some(None);
    assert!(show(nested) == "Some(None)", show(nested));
}

test("show of Result") {
    assert!(show(half(4)) == "Ok(2)", show(half(4)));
    assert!(show(half(3)) == "Err(odd)", show(half(3)));
    let r: Result<float, int> = Err(7);
    assert!(r.show() == "Err(7)", r.show());
    let ok: Result<(), string> = Ok(());
    assert!(show(ok) == "Ok(())", show(ok));
}

test("format %v of Option and Result") {
    let o: Option<int> = Some(1);
    assert!(format("%v", o) == "Some(1)", format("%v", o));
    assert!(
        format("%v / %v", find(0), half(6)) == "None / Ok(3)",
        format("%v / %v", find(0), half(6)),
    );
    assert!(format("%v", (find(2), 1)) == "(Some(2), 1)", format("%v", (find(2), 1)));
    let xs: [Option<int>; 2] = [Some(1), None];
    assert!(format("%v", xs) == "[Some(1), None]", format("%v", xs));
    assert!(show(xs) == "[Some(1), None]", show(xs));
}

test("Option and Result meet a Show bound") {
    assert!(shown(find(5)) == "Some(5)", shown(find(5)));
    assert!(shown_method(find(-1)) == "None", shown_method(find(-1)));
    assert!(shown(half(1)) == "Err(odd)", shown(half(1)));
    assert!(shown((find(1), half(2))) == "(Some(1), Ok(1))", shown((find(1), half(2))));
}

test("Option and Result meet a Show bound through a shared body") {
    let s = shown;
    let m = shown_method;
    assert!(s(find(5)) == "Some(5)", s(find(5)));
    assert!(m(find(0)) == "None", m(find(0)));
    assert!(s(half(8)) == "Ok(4)", s(half(8)));
    let p: Option<string> = Some("x");
    assert!(s(p) == "Some(x)", s(p));
    let r: Result<(), string> = Err("e");
    assert!(m(r) == "Err(e)", m(r));
}
