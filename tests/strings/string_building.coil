// Building strings: the empty string is the identity of `+`, `+=` in a loop
// accumulates, and built strings compare by content wherever they are stored.
use tests::assert;
use string::format;

fn repeat(string s, int n) -> string {
    let out = "";
    let i = 0;
    while i < n {
        out += s;
        i = i + 1;
    }
    return out;
}

test("empty string is the identity") {
    let s = "abc";
    assert!(s + "" == s);
    assert!("" + s == s);
    assert!("" + "" == "");
    let both = "" + "";
    assert!(both.len() == 0);
}

test("plus-assign accumulates in a loop") {
    assert!(repeat("ab", 3) == "ababab");
    assert!(repeat("x", 0) == "");
    assert!(repeat("é", 4).len() == 8);
}

test("concatenation is associative and not commutative") {
    let a = "a";
    let b = "b";
    let c = "c";
    assert!((a + b) + c == a + (b + c));
    assert!(a + b != b + a);
}

test("built strings in a Vec and a dict compare by content") {
    let v: Vec<string> = Vec::new();
    v.push(repeat("z", 2));
    v.push(format("%i", 12));
    assert!(v[0] == "zz");
    assert!(v[1] == "1" + "2");
    let d = { name: "a" + "b" };
    assert!(d.name == "ab");
    d.name += "c";
    assert!(d.name == repeat("abc", 1));
}

test("strings are immutable values") {
    let a = "x";
    let b = a;
    b += "y";
    assert!(a == "x");
    assert!(b == "xy");
}
