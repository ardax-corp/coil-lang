// Indexing a niche `Option` / `Result` item out of a tuple that is not a
// literal reads back the niche word `make_tuple` stored, not an internal
// compiler error (#1614).
use tests::assert;
use string::format;

fn mk() -> (Option<string>, int) {
    return (Some("a"), 1);
}

fn mk_none() -> (Option<string>, int) {
    return (None, 2);
}

fn first((Option<string>, int) t) -> Option<string> {
    return t[0];
}

fn checked(int n) -> Result<string, string> {
    if n < 0 {
        raise "negative";
    }
    return "ok";
}

fn pair(int n) -> (int, Result<string, string>) {
    return (n, checked(n));
}

fn second((int, Result<string, string>) t) -> Result<string, string> {
    return t[1];
}

test("a Some item read from a parameter") {
    let o = first(mk());
    let s = match o {
        Some(v) => v,
        None => "none",
    };
    assert!(s == "a", s);
}

test("a None item read from a parameter") {
    assert!(first(mk_none()) == None, "expected None");
}

test("niche Result items") {
    assert!(format("%v", second(pair(1))) == "Ok(ok)", "ok");
    assert!(format("%v", second(pair(-1))) == "Err(negative)", "err");
}

test("a record Option item read from an array's tuple") {
    let ts = [(Some({ x: 7 }), "b"), (None, "c")];
    let out = "";
    for i in [0, 1] {
        let t = ts[i];
        out += match t[0] {
            Some(r) => format("%i;", r.x),
            None => "none;",
        };
    }
    assert!(out == "7;none;", out);
}
