// String and int literals take a byte type from an annotation or a cast:
// `Vec<byte>` from a string literal or a list of one-byte literals, `byte`
// from a one-byte string or an in-range int, and `"…" as Vec<byte>`; a
// `byte` shows as its number and a `Vec<byte>` is a `write` buffer.
use tests::assert;
use io::{stdin, write};

// Compiles: a `Vec<byte>` local is a valid `write` buffer (not called, so
// nothing is written to stdin).
fn write_bytes() {
    let data = Vec::from([1 as byte, 2 as byte]);
    write(stdin(), data);
}

test("Vec<byte> from a string literal") {
    let b: Vec<byte> = "Hi";
    assert!((typeof b) == "prelude::Vec<byte>");
    assert!(len(b) == 2);
    assert!(b[0] == "H");
}

test("Vec<byte> from one-byte string literals") {
    let buf: Vec<byte> = ["a", "b", "\n"];
    assert!((typeof buf) == "prelude::Vec<byte>");
    assert!((buf[2] as int) == 10);
}

test("byte from a one-byte string and an in-range int") {
    let b: byte = "/";
    assert!((typeof b) == "byte");
    assert!(b == "/");
    let c: byte = 42;
    assert!((typeof c) == "byte");
    assert!((c as int) == 42);
}

test("a string literal cast to Vec<byte>") {
    let b = "hi" as Vec<byte>;
    assert!((typeof b) == "prelude::Vec<byte>");
    assert!(len(b) == 2);
}

test("byte has a Show instance") {
    let b: byte = 42;
    assert!(show(b) == "42");
}
