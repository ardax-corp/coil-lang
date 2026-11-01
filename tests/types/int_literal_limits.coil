// `int` is 64-bit signed: the largest literal is 9223372036854775807, the
// smallest value is reached as `-9223372036854775807 - 1`, and values past
// 32 bits keep their exact value through arithmetic, calls and comparisons.
use tests::assert;

fn id(int x) -> int {
    return x;
}

test("max literal") {
    let max = 9223372036854775807;
    assert!(id(max) == max);
    assert!(max - 1 == 9223372036854775806);
    assert!(max > 0);
    assert!(max / 2 == 4611686018427387903);
}

test("min value") {
    let min = -9223372036854775807 - 1;
    assert!(min < -9223372036854775807);
    assert!(id(min) + 1 == -9223372036854775807);
    assert!(min / 2 == -4611686018427387904);
    assert!(min % 10 == -8);
}

test("values just past 32 bits") {
    let a = 4294967296;
    assert!(id(a) == 4294967295 + 1);
    assert!(a * 2 == 8589934592);
    assert!(id(2147483647) + 1 == 2147483648);
    assert!(id(-2147483648) - 1 == -2147483649);
    assert!(a > 4294967295);
}

test("bitwise on wide values") {
    let x = 1 << 40;
    assert!(x == 1099511627776);
    assert!(x | 1 == 1099511627777);
    assert!(~0 == -1);
    assert!(id(-1) & 9223372036854775807 == 9223372036854775807);
}
