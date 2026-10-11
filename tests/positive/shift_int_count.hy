// A shift count is an `int` whatever the type being shifted, in `<<` /
// `>>` and in `<<=` / `>>=` (#883). Byte shifts wrap modulo 256 (#882).

fn shl_only<T: Shl>(T a, int n) -> T {
    return a << n;
}

fn shr_in_place<T: Shr>(T a, int n) -> T {
    a >>= n;
    return a;
}

test("byte shifted by an int") {
    let a = 7 as byte;
    assert((a << 1) == (14 as byte))?;
    assert((a << 6) == (192 as byte))?;
    assert((a >> 1) == (3 as byte))?;
}

test("compound shifts on a byte") {
    let a = 7 as byte;
    a <<= 6;
    assert(a == (192 as byte))?;
    a >>= 4;
    assert(a == (12 as byte))?;
}

test("compound shifts on an int") {
    let n = 3;
    n <<= 4;
    assert(n == 48)?;
    n >>= 2;
    assert(n == 12)?;
}

test("bound type parameter shifted by an int") {
    assert(shl_only(1, 4) == 16)?;
    assert(shl_only(3 as byte, 2) == (12 as byte))?;
    assert(shr_in_place(64, 3) == 8)?;
    assert(shr_in_place(64 as byte, 3) == (8 as byte))?;
}
