// `~x` is bitwise NOT on an `int` (two's complement: `~x == -x - 1`), not
// logical `!`, including combined with `&`.
use tests::assert;

fn flip(int a) -> int {
    return ~a;
}

fn flip_mask(int a, int mask) -> int {
    return ~a & mask;
}

test("bitwise not in a call") {
    assert!(flip(5) == -6);
    assert!(flip(0) == -1);
    assert!(flip(-1) == 0);
    assert!(flip_mask(12, 15) == 3);
}
