// Shifts never trap: `<<` by 0..=63 may move bits into or past the sign
// bit without an overflow panic, and `>>` is arithmetic (it copies the
// sign bit). Counts of 64 and above are a known VM bug (findings).
use tests::assert;

fn shl(int x, int n) -> int {
    return x << n;
}

fn shr(int x, int n) -> int {
    return x >> n;
}

test("counts below 64") {
    assert!(shl(1, 0) == 1);
    assert!(shl(1, 1) == 2);
    assert!(shl(1, 62) == 4611686018427387904);
    assert!(shr(1024, 10) == 1);
    assert!(shr(1023, 10) == 0);
}

test("shifting into the sign bit does not trap") {
    assert!(shl(1, 63) == -9223372036854775807 - 1);
    assert!(shl(3, 63) == -9223372036854775807 - 1);
    assert!(shl(-1, 63) == -9223372036854775807 - 1);
    assert!(shl(1, 63) < 0);
}

test("right shift is arithmetic") {
    assert!(shr(-8, 1) == -4);
    assert!(shr(-1, 1) == -1);
    assert!(shr(-1, 63) == -1);
    assert!(shr(-9223372036854775807 - 1, 63) == -1);
    assert!(shr(9223372036854775807, 63) == 0);
    assert!(shr(-7, 1) == -4);
}

test("compound shifts") {
    let x = 1;
    x <<= 10;
    assert!(x == 1024);
    x >>= 3;
    assert!(x == 128);
    x <<= 56;
    assert!(x == -9223372036854775807 - 1);
}

test("shift binds looser than additive") {
    assert!(1 << 2 + 1 == 8);
    assert!(16 >> 1 + 1 == 4);
    assert!((1 << 2) + 1 == 5);
}
