use tests::assert;
use string::to_bytes;

test("non-literal string via to_bytes") {
    let s = "hi";
    let b = to_bytes(s);
    assert!(b == to_bytes("hi"));
    assert!(b.len() == 2);
}

test("a string literal casts to fixed bytes") {
    let b = "hi" as [byte; 2];
    assert!(b.len() == 2);
    assert!((b[1] as int) == 105);
}

test("literal cast to Vec<byte>") {
    let b = "ok" as Vec<byte>;
    assert!(b.len() == 2);
    assert!(b == to_bytes("ok"));
}
