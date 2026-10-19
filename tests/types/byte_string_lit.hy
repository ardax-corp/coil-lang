use tests::assert;

test("string literal coerces to byte") {
    let slash: byte = "/";
    let nl: byte = "\n";
    assert!((slash as int) == 47);
    assert!((nl as int) == 10);
    assert!(slash == "/");
    let buf: [byte; 3] = ["H", "i", "\n"];
    assert!((buf[0] as int) == 72);
    assert!(("." as byte) as int == 46);
}

test("escaped quote string literal as byte") {
    let q: byte = "\"";
    assert!((q as int) == 34);
}

test("string literal coerces to byte vec") {
    let a = "Hi" as Vec<byte>;
    assert!(a.len() == 2);
    assert!((a[0] as int) == 72);
    assert!((a[1] as int) == 105);
    let b: [byte; 3] = "ab\n";
    assert!(b.len() == 3);
    let c = "xy" as Vec<byte>;
    assert!(c.len() == 2);
    let d = "ok" as [byte; 2];
    assert!((d[0] as int) == 111);
}
