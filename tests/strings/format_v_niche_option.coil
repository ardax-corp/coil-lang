// `%v` of an `Option` or `Result` held as its niche word (a record, string
// or tuple payload) calls the instance's raw `show` on that word, as
// `show(o)` does: `Some({ x: 1 })`, not an internal compiler error (#1591).
use tests::assert;
use string::format;

fn checked(int n) -> Result<string, string> {
    if n < 0 {
        raise "negative";
    }
    return "ok";
}

test("Some of a record") {
    let o = Some({ x: 1 });
    let s = format("%v", o);
    assert!(s == "Some({ x: 1 })", s);
}

test("Some of a record with several fields") {
    let s = format("%v", Some({ x: 1, y: "b" }));
    assert!(s == "Some({ x: 1, y: b })", s);
}

test("None of a record type") {
    let n = match 0 {
        1 => Some({ x: 1 }),
        default => None,
    };
    let s = format("%v", n);
    assert!(s == "None", s);
}

test("a record Option chosen at run time") {
    let out = "";
    for n in [2, 0] {
        let o = match n {
            0 => None,
            default => Some({ x: n }),
        };
        out += format("%v;", o);
    }
    assert!(out == "Some({ x: 2 });None;", out);
}

test("Some of a string and of a tuple") {
    let s = format("%v %v", Some("a"), Some((1, 2)));
    assert!(s == "Some(a) Some((1, 2))", s);
}

test("nested and in an array") {
    let s = format("%v", Some(Some({ x: 1 })));
    assert!(s == "Some(Some({ x: 1 }))", s);
    let a = format("%v", [Some({ x: 1 })]);
    assert!(a == "[Some({ x: 1 })]", a);
}

test("a niche Result") {
    let s = format("%v %v", checked(1), checked(-1));
    assert!(s == "Ok(ok) Err(negative)", s);
}
