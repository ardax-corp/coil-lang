// `x & 0xFFFFFFFF` keeps the low 32 bits of a 64-bit int; only `-1` is the
// all-ones identity for `&` (#860).

fn big() -> int {
    return 9223372036854775807;
}

fn mask(int a) -> int {
    return a & 4294967295;
}

fn either(int a) -> int {
    return a | 4294967295;
}

test("32-bit masks are not identities") {
    let a = big();
    assert(mask(a) == 4294967295, "and")?;
    assert((a & 4294967295) >> 16 == 65535, "and shift")?;
    assert(mask(0 - 1) == 4294967295, "and neg")?;
    assert(either(0 - 4294967296) == 0 - 1, "or")?;
    assert(a & (0 - 1) == a, "and -1")?;
}
