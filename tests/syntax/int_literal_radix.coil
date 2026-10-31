// Integer literals take a `0x`, `0o` or `0b` prefix and `_` between
// digits (#1849).
use tests::assert;

const MASK: int = 0xff_ff;

test("radix prefixes") {
    assert!(0xff == 255);
    assert!(0xFF == 255);
    assert!(0o17 == 15);
    assert!(0b1010 == 10);
    assert!(0x0 == 0);
    assert!(0x7fff_ffff_ffff_ffff == 9_223_372_036_854_775_807);
    assert!(-0x10 == -16);
    assert!(MASK == 65535);
}

test("digit separators") {
    assert!(1_000_000 == 1000000);
    assert!(0b1111_0000 == 240);
    let b: byte = 0x7f;
    assert!((b as int) == 127);
}

test("patterns and array lengths") {
    let n = 0x10;
    let hit = match n {
        0x0f => 1,
        0x10 => 2,
        -0b1 => 3,
        default => 4,
    };
    assert!(hit == 2);
    let a: [int; 0x3] = [1, 2, 3];
    assert!(a.len() == 3);
}
