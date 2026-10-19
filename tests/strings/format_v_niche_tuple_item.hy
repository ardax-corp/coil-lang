// `%v` of a tuple holding a niche `Option` / `Result` item shows the item
// as `show` does, `(Some(a), 1)`, not an internal compiler error (#1611).
use tests::assert;
use string::format;

fn checked(int n) -> Result<string, string> {
    if n < 0 {
        raise "negative";
    }
    return "ok";
}

fn pair<T>(T a) -> (T, int) {
    return (a, 1);
}

fn opt(int n) -> Option<string> {
    if n > 0 {
        return Some("a");
    }
    return None;
}

test("a tuple with a Some of a string") {
    let s = format("%v", (Some("a"), 1));
    assert!(s == "(Some(a), 1)", s);
}

test("a tuple with a None item") {
    let n = match 0 {
        1 => Some("a"),
        default => None,
    };
    let s = format("%v", (1, n));
    assert!(s == "(1, None)", s);
}

test("a tuple local with a record Option") {
    let t = (Some({ x: 1 }), "b");
    let s = format("%v", t);
    assert!(s == "(Some({ x: 1 }), b)", s);
}

test("a tuple with niche Results") {
    let s = format("%v", (checked(1), checked(-1)));
    assert!(s == "(Ok(ok), Err(negative))", s);
}

test("a tuple chosen at run time") {
    let out = "";
    for n in [2, 0] {
        let o = match n {
            0 => None,
            default => Some(n),
        };
        out += format("%v;", (o, n));
    }
    assert!(out == "(Some(2), 2);(None, 0);", out);
}

test("an array of tuples with Option items") {
    let s = format("%v", [(Some("a"), 1), (None, 2)]);
    assert!(s == "[(Some(a), 1), (None, 2)]", s);
}

test("an array local of Options") {
    let n = match 0 {
        1 => Some("b"),
        default => None,
    };
    let v = [Some("a"), n];
    let s = format("%v", v);
    assert!(s == "[Some(a), None]", s);
}

test("a tuple made by a generic function") {
    let s = format("%v %v", pair(opt(1)), pair(opt(0)));
    assert!(s == "(Some(a), 1) (None, 1)", s);
    let r = format("%v", (pair(opt(1)), opt(1)));
    assert!(r == "((Some(a), 1), Some(a))", r);
}
