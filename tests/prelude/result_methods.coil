// The core prelude's `Result` methods, on immediate, niche and unit
// payloads, and through a shared generic body.
use tests::assert;

fn parse_digit(string s) -> Result<int, string> {
    if s == "1" {
        return 1;
    }
    raise "not a digit: " + s;
}

fn positive(int x) -> Result<int, string> {
    if x > 0 {
        return x;
    }
    raise "not positive";
}

fn twice<T, E>(Result<T, E> r, T -> T f) -> Result<T, E> {
    return r.map(f).map(f);
}

test("is_ok and is_err") {
    let a: Result<int, string> = Ok(1);
    let b: Result<int, string> = Err("no");
    assert!(a.is_ok() && !a.is_err());
    assert!(b.is_err() && !b.is_ok());
}

test("ok and err") {
    let a: Result<int, string> = Ok(1);
    let b: Result<int, string> = Err("no");
    assert!(a.ok() == Some(1));
    assert!(a.err() == None);
    assert!(b.ok() == None);
    assert!(b.err() == Some("no"));
}

test("unwrap_or and unwrap_or_else") {
    assert!(parse_digit("1").unwrap_or(0) == 1);
    assert!(parse_digit("x").unwrap_or(0) == 0);
    assert!(parse_digit("x").unwrap_or_else(fn (string e) => e.len()) == 14);
}

test("map and map_err") {
    assert!(parse_digit("1").map(fn (int x) => x + 1) == Ok(2));
    assert!(parse_digit("x").map(fn (int x) => x + 1) == Err("not a digit: x"));
    assert!(parse_digit("x").map_err(fn (string e) => e.len()) == Err(14));
    assert!(parse_digit("1").map_err(fn (string e) => e.len()) == Ok(1));
}

test("and_then and or_else") {
    assert!(parse_digit("1").and_then(positive) == Ok(1));
    assert!(parse_digit("x").and_then(positive) == Err("not a digit: x"));
    let r: Result<int, string> = Err("bad");
    assert!(r.or_else(fn (string e) => positive(e.len())) == Ok(3));
}

test("unit and string payloads") {
    let u: Result<(), string> = Ok(());
    assert!(u.is_ok());
    let s: Result<string, string> = Ok("a");
    assert!(s.map(fn (string t) => t + "b") == Ok("ab"));
}

test("shared generic bodies") {
    assert!(twice(Ok(1), fn (int x) => x * 3) == Ok(9));
    let e: Result<int, string> = Err("e");
    assert!(twice(e, fn (int x) => x * 3) == Err("e"));
}
