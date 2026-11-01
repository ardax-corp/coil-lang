// Escapes in string literals decode to single bytes: `\"`, `\\`, `\n`,
// `\t` and `\x41`; they print and compare as the decoded text.
use tests::assert;
use string::{format, to_bytes};

test("escapes decode to single bytes") {
    let q = "\"";
    let bs = "\\";
    let nl = "\n";
    assert!(q.len() == 1 && bs.len() == 1 && nl.len() == 1);
    assert!((to_bytes(q)[0] as int) == 34);
    assert!((to_bytes(bs)[0] as int) == 92);
    assert!((to_bytes(nl)[0] as int) == 10);
    assert!((to_bytes("a\tb")[1] as int) == 9);
}

test("hex escape") {
    assert!("\x41" == "A");
    assert!("\x41\x42" == "AB");
}

test("escapes inside format") {
    assert!(format("[%s]", "a\"b") == "[a\"b]");
    let s = format("%s\n%s", "a", "b");
    let t = s;
    assert!(t.len() == 3);
    assert!(t[1] == "\n");
}
