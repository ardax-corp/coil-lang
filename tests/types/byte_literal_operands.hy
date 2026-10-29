// An `int` literal in 0..=255 next to a `byte` operand of a binary
// operator types as `byte`, as it does under an expected `byte` in a
// `let`, an argument or a return (#2093). A single-byte string literal
// coerces the same way in arithmetic, as it already did in `==`.
use tests::assert;
use string::to_bytes;

fn widen(byte b) -> int {
    return b as int;
}

test("arithmetic with a literal on either side") {
    let b = to_bytes("a")[0];
    let upper = b - 32;
    assert!(widen(upper) == 65);
    assert!(widen(32 + b) == 129);
    assert!(widen(b * 2) == 194);
    assert!(widen(b / 2) == 48);
    assert!(widen(b % 10) == 7);
    assert!(widen(b ** 1) == 97);
    assert!(widen(255 - b) == 158);
    assert!(widen(b & 15) == 1);
    assert!(widen(b | 128) == 225);
    assert!(widen(b ^ 255) == 158);
}

test("comparisons with a literal") {
    let b = to_bytes("0")[0];
    assert!(b == 48);
    assert!(48 == b);
    assert!(b != 49);
    assert!(b < 128);
    assert!(b <= 48);
    assert!(b > 47);
    assert!(b >= 48);
    assert!(0 < b);
}

test("compound assignment with a literal") {
    let b = to_bytes("c")[0];
    b -= 1;
    assert!(widen(b) == 98);
    b += 2;
    assert!(widen(b) == 100);
    b *= 2;
    assert!(widen(b) == 200);
    b /= 4;
    assert!(widen(b) == 50);
    b %= 7;
    assert!(widen(b) == 1);
    b |= 6;
    assert!(widen(b) == 7);
    b &= 5;
    assert!(widen(b) == 5);
    b ^= 1;
    assert!(widen(b) == 4);
}

test("single-byte string literals in arithmetic") {
    let b = to_bytes("7")[0];
    let digit = b - "0";
    assert!(widen(digit) == 7);
    assert!(widen("0" + digit) == 55);
    assert!(b > "0");
    assert!(b <= "9");
}

test("a while over the bytes of a string") {
    let bytes = to_bytes("Hello, World 42");
    let i = 0;
    let digits = 0;
    let uppers = 0;
    let lowered = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c >= 48 && c <= 57 {
            digits = digits * 10 + widen(c - 48);
        } else if c >= "A" && c <= "Z" {
            uppers += 1;
            lowered += widen(c + 32);
        }
        i += 1;
    }
    assert!(digits == 42);
    assert!(uppers == 2);
    assert!(lowered == 104 + 119);
}

test("a literal result that fits stays a byte") {
    let b = to_bytes("z")[0];
    let k: byte = b - 25 + 1;
    assert!(k == "b");
    let v: Vec<byte> = [b, b - 1];
    assert!(v[1] + 1 == b);
}
