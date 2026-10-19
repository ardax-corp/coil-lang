// A call through a `PolyFn` value may take and return a niche enum, an
// `Option` or a `Result`, as its bare type parameter (#1515).
use tests::assert;

fn id<T>(T x) -> T {
    return x;
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
