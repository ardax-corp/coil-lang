// `Option` and `Result` are `Hash` when their payloads are (#1906): equal
// values hash equal, through a bound too, and a derived `Hash` takes an
// `Option` field. A variant hashes as a derived enum's does, seeded with
// its index: `None` is 0, `Some(x)` is `31 ^ x.hash()`, `Ok(x)` is
// `x.hash()` and `Err(e)` is `31 ^ e.hash()`.
use tests::assert;

fn hashed<W: Hash>(W w) -> int {
    return w.hash();
}

#[derive(Hash)]
class Tagged {
    pub name: string,
    pub note: Option<string>,
}

test("option hashes by its payload") {
    let a: Option<string> = Option::Some("one");
    let b: Option<string> = Option::Some("one");
    let c: Option<string> = Option::Some("two");
    let none: Option<string> = Option::None;
    assert!(hashed(a) == hashed(b));
    assert!(hashed(a) != hashed(c));
    assert!(hashed(a) == (31 ^ "one".hash()));
    assert!(none.hash() == 0);
    let n: Option<int> = Option::Some(5);
    assert!(n.hash() == (31 ^ 5.hash()));
    let nested: Option<Option<string>> = Option::Some(Option::None);
    assert!(hashed(nested) == 31);
}

test("result hashes by its variant and payload") {
    let ok: Result<string, string> = Result::Ok("x");
    let err: Result<string, string> = Result::Err("x");
    assert!(hashed(ok) == "x".hash());
    assert!(hashed(err) == (31 ^ "x".hash()));
    assert!(hashed(ok) != hashed(err));
    let unit: Result<(), string> = Result::Err("bad");
    assert!(hashed(unit) == (31 ^ "bad".hash()));
}

test("derived hash takes an option field") {
    let a = new Tagged("a", Option::Some("n"));
    let b = new Tagged("a", Option::Some("n"));
    let c = new Tagged("a", Option::None);
    assert!(a.hash() == b.hash());
    assert!(a.hash() != c.hash());
}
