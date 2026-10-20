// `Option` / `Result` methods and their lambdas spliced into the caller:
// a `map` whose lambda is inlined too, in a loop, with each arm taken.
use tests::assert;

fn lookup(int i) -> Option<int> {
    if i % 3 == 0 {
        return None;
    }
    return Some(i);
}

fn checked(int i) -> Result<int, string> {
    if i < 0 {
        return Err("negative");
    }
    return Ok(i);
}

test("map and unwrap_or with an inlined lambda") {
    let acc = 0;
    let i = 0;
    while i < 10 {
        acc = acc + lookup(i).map(fn (int x) => x * 10).unwrap_or(1);
        i = i + 1;
    }
    // 1 + 2 + 4 + 5 + 7 + 8 = 27, times 10, plus 4 Nones.
    assert!(acc == 274);
}

test("a lambda local called after an inlined map") {
    let f = fn (int x) => x + 1;
    let a = Some(4).map(f).unwrap_or(0);
    assert!(a == 5);
    assert!(f(a) == 6);
}

test("string payloads") {
    let s = Some("ab").map(fn (string x) => x + "c").unwrap_or("");
    assert!(s == "abc");
    let n: Option<string> = None;
    assert!(n.map(fn (string x) => x + "c").unwrap_or("none") == "none");
}

test("Result map with an inlined lambda") {
    assert!(checked(2).map(fn (int x) => x * 3).unwrap_or(0) == 6);
    assert!(checked(-1).map(fn (int x) => x * 3).unwrap_or(0) == 0);
}
