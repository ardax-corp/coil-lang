// String literal escapes: `\\` `\"` `\n` `\r` `\t` `\0` `\e` `\xHH` and
// `\u{HEX}` each decode to the bytes they name (UTF-8 for `\u{…}`).
use tests::assert;
use string::to_bytes;

fn byte_at(string s, int i) -> int {
    return to_bytes(s)[i] as int;
}

fn nbytes(string s) -> int {
    return len(to_bytes(s));
}

test("single-character escapes") {
    assert!(byte_at("\\", 0) == 92);
    assert!(byte_at("\"", 0) == 34);
    assert!(byte_at("\n", 0) == 10);
    assert!(byte_at("\r", 0) == 13);
    assert!(byte_at("\t", 0) == 9);
    assert!(byte_at("\0", 0) == 0);
    assert!(byte_at("\e", 0) == 27);
    assert!(nbytes("\\\"\n\r\t\0\e") == 7);
}

test("hex escapes") {
    assert!(byte_at("\x41", 0) == 65);
    assert!(byte_at("\x7f", 0) == 127);
    assert!(byte_at("\x00", 0) == 0);
    assert!("\x41\x42" == "AB");
    assert!(nbytes("a\x20b") == 3);
}

test("unicode escapes encode UTF-8") {
    assert!("\u{41}" == "A");
    assert!(nbytes("\u{e9}") == 2);
    assert!("\u{e9}" == "é");
    assert!(byte_at("\u{e9}", 0) == 195);
    assert!(byte_at("\u{e9}", 1) == 169);
    assert!(nbytes("\u{20ac}") == 3);
    assert!(nbytes("\u{1F600}") == 4);
}

test("escapes next to plain text") {
    let s = "say \"hi\"\tnow";
    assert!(nbytes(s) == 12);
    assert!(byte_at(s, 4) == 34);
    assert!(byte_at(s, 8) == 9);
    assert!("a\\nb" != "a\nb");
    assert!(nbytes("a\\nb") == 4);
}
