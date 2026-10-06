// String-to-bytes casts and array `==` / `!=` lower from HIR.
fn same(string a, string b) -> bool {
    return (a as [byte]) == (b as [byte]);
}

test("string as bytes") {
    let s = "hey";
    let b = s as [byte];
    assert(len(b) == 3)?;
    assert((b[0] as int) == 104)?;
    assert(b == ("hey" as [byte]))?;
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
