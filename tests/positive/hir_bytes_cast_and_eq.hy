// String-to-bytes conversions and array `==` / `!=` lower from HIR.
use string::to_bytes;

fn same(string a, string b) -> bool {
    return to_bytes(a) == to_bytes(b);
}

test("string as bytes") {
    let s = "hey";
    let b = to_bytes(s);
    assert(len(b) == 3)?;
    assert((b[0] as int) == 104)?;
    assert(b == to_bytes("hey"))?;
}

test("literal as Vec<byte>") {
    let b = "a\n" as Vec<byte>;
    assert(len(b) == 2)?;
    assert((b[1] as int) == 10)?;
}

test("array equality") {
    assert(same("ab", "ab"))?;
    assert(!same("ab", "ac"))?;
    let xs = [1, 2, 3];
    let ys = [1, 2, 3];
    assert(xs == ys)?;
    assert(xs != [1, 2, 4])?;
}
