// `\e` in a string literal is the ESC byte (27), so ANSI colour sequences
// can be written inline.
use tests::assert;
use string::to_bytes;

test("an ANSI colour sequence") {
    let s = "\e[31mred\e[0m";
    let b = to_bytes(s);
    assert!(b.len() == 12);
    assert!(b[0] == (27 as byte));
    assert!(b[8] == (27 as byte));
    assert!(s == "\x1b[31mred\x1b[0m");
}
