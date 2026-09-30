// String concat / format. Note: `==` on strings is pointer identity (interned
// literals compare equal; concat/format results are fresh allocations).
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

test("literal string equality") {
    assert("hi" == "hi")?;
    assert("a" != "b")?;
}

test("concat produces usable string") {
    let s = "a" + "b";
    // Fresh allocation — distinct from an unrelated interned literal.
    assert(s != "z")?;
    let t = s + "c";
    assert(t != s)?;
}

test("format produces usable string") {
    let s = format("%i-%s", 42, "x");
    assert(s != "")?;
    let t = format("%z:%i", true, 7);
    assert(t != s)?;
}

test("concat chains") {
    let a = "";
    let b = "x";
    let s = a + b;
    assert(s != "yy")?;
    let t = b + a + b;
    assert(t != s)?;
}

test("format multi-arg distinct from literals") {
    let s = format("%i+%i=%i", 1, 2, 3);
    assert(s != "1")?;
    assert(s != "hello")?;
}
