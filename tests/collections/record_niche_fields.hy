// A record can hold a field of niche layout (an `Option` of a heap value,
// a niche `Result`): it is stored as its niche word, as a tuple item is,
// and reads, writes, matches, compares and shows as the value (#1613).
use tests::assert;
use string::format;

fn checked(int n) -> Result<string, string> {
    if n < 0 {
        raise "negative";
    }
    return "ok";
}

fn id<T>(T x) -> T {
    return x;
}

fn pick(int n) -> Option<string> {
    if n == 0 {
        return None;
    }
    return Some(format("n%i", n));
}

test("a Some of a string field read and matched") {
    let r = { a: Some("a"), b: 2 };
    let s = match r.a {
        Some(s) => s,
        None => "none",
    };
    assert!(s == "a", s);
    assert!(r.b == 2);
}

test("a None field and assignment") {
    let r = { name: "x", tag: pick(0) };
    assert!(r.tag == None, "expected None");
    r.tag = pick(3);
    assert!((r.tag ?? "") == "n3", r.tag ?? "");
    r.tag = None;
    assert!((r.tag ?? "none") == "none");
}

test("a record Option field") {
    let r = { a: Some({ x: 1 }) };
    let x = match r.a {
        Some(inner) => inner.x,
        None => -1,
    };
    assert!(x == 1);
}

test("niche Result fields") {
    let r = { good: checked(1), bad: checked(-1) };
    let g = match r.good {
        Ok(s) => s,
        Err(e) => e,
    };
    let b = match r.bad {
        Ok(s) => s,
        Err(e) => e,
    };
    assert!(g == "ok" && b == "negative", g + b);
}

test("format of a record with niche fields") {
    let s = format("%v", { a: Some({ x: 1 }) });
    assert!(s == "{ a: Some({ x: 1 }) }", s);
    let t = format("%v", { b: pick(0), a: Some("a"), c: checked(-1) });
    assert!(t == "{ a: Some(a), b: None, c: Err(negative) }", t);
}

test("equality of niche fields") {
    let p = { a: pick(1), b: 1 };
    let q = { b: 1, a: pick(1) };
    let n = { a: pick(0), b: 1 };
    assert!(p.a == q.a, "equal");
    assert!(p.a != n.a, "differ");
    assert!(n.a == None, "none");
    let r = { e: checked(-1), o: checked(2) };
    assert!(r.e == checked(-2) && r.o == checked(1), "results");
    assert!(r.e != r.o, "ok and err");
}

test("patterns, generics and iteration") {
    let r = { o: pick(2), k: 7 };
    let { o, k } = r;
    assert!((o ?? "") == "n2" && k == 7);
    let s = id(r);
    assert!((s.o ?? "") == "n2");
    let rows = [{ o: pick(0) }, { o: pick(5) }];
    let out = "";
    for row in rows {
        out += row.o ?? "-";
    }
    assert!(out == "-n5", out);
    assert!(format("%v", rows) == "[{ o: None }, { o: Some(n5) }]", format("%v", rows));
}

fn show_it<T: Show>(T x) -> string {
    return format("%v", x);
}

test("iteration over niche fields and a generic show") {
    let r = { b: pick(0), a: pick(4) };
    let out = "";
    for (k, v) in r {
        out += k + "=" + (v ?? "-") + ";";
    }
    assert!(out == "a=n4;b=-;", out);
    assert!(show_it(r) == "{ a: Some(n4), b: None }", show_it(r));
}
