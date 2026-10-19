// An `int` literal in 0..=255 becomes a `byte` where a `byte` is expected:
// a return, an annotated `let`, a call argument, and `Vec<byte>` /
// `[byte; N]` elements; literal arithmetic under that expectation types as
// `byte` too.
use tests::assert;

fn two() -> byte {
    return 1 + 1;
}

fn top() -> byte {
    return 255;
}

fn widen(byte b) -> int {
    return b as int;
}

test("returns and annotated lets") {
    assert!(widen(two()) == 2);
    assert!(widen(top()) == 255);
    let z: byte = 0;
    assert!(widen(z) == 0);
}

test("call arguments") {
    assert!(widen(42) == 42);
    assert!(widen(255) == 255);
    assert!(widen("a") == 97);
}

test("array and Vec elements") {
    let fixed: [byte; 3] = [1, 128, 255];
    assert!(widen(fixed[1]) == 128);
    assert!(widen(fixed[2]) == 255);
    let v: Vec<byte> = [0, 10, 20];
    v.push(30);
    assert!(v.len() == 4);
    assert!(widen(v[3]) == 30);
}

test("one-byte string literals and escapes") {
    assert!(widen("\n") == 10);
    assert!(widen("\t") == 9);
    assert!(widen("\0") == 0);
    assert!(widen("\x41") == 65);
    assert!(widen("\"") == 34);
    assert!(widen("\\") == 92);
    let b: byte = "/";
    assert!(b == "/");
}
