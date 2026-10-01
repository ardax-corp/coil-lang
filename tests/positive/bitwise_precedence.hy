// #596: shifts bind tighter than `&`, then `^`, then `|`, all above the
// comparisons and left-associative (Rust order).

fn bits(int one, int two, int three) -> int {
    return one << two | three;
}

test("shift and bitwise operators follow Rust precedence") {
    let one = 1;
    let two = 2;
    let three = 3;
    let six = 6;
    let eight = 8;
    let twelve = 12;
    assert(one << two | one == 5, "1 << 2 | 1")?;
    assert(six & three | eight == 10, "6 & 3 | 8")?;
    assert(twelve >> one & one == 0, "12 >> 1 & 1")?;
    assert(six & one << two == 4, "6 & (1 << 2)")?;
    assert(six ^ three & one == 7, "6 ^ (3 & 1)")?;
    assert(one | two ^ three == 1, "1 | (2 ^ 3)")?;
    assert(bits(1, 4, 1) == 17, "runtime operands")?;
}

test("shifts associate left") {
    let one = 1;
    let two = 2;
    let three = 3;
    let eight = 8;
    assert(one << two << three == 32, "(1 << 2) << 3")?;
    assert(eight >> one >> one == 2, "(8 >> 1) >> 1")?;
}

test("xor binds tighter than comparisons and logic") {
    let one = 1;
    let two = 2;
    assert(one ^ two == 3, "(1 ^ 2) == 3")?;
    assert(one == 1 && one ^ two == 3, "&& over ==")?;
}

test("constant folding keeps the grouping") {
    assert(1 << 2 | 1 == 5, "1 << 2 | 1")?;
    assert(1 << 2 << 3 == 32, "1 << 2 << 3")?;
    assert(6 & 3 | 8 == 10, "6 & 3 | 8")?;
}
