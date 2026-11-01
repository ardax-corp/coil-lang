// A `const` string folded at compile time from `+` decodes every escape
// a plain literal does, so its value and length match the unfolded string
// (#1810).
use tests::assert;

const QUOTE_SLASH: string = "a\"b" + "\\c";

const HEX_UNI: string = "\x41" + "\u{e9}" + "\e";

const MIXED: string = "\t\"" + "\\n";

test("folded const decodes quote and backslash escapes") {
    assert!(QUOTE_SLASH == "a\"b\\c");
    assert!(QUOTE_SLASH.len() == 5);
}

test("folded const decodes hex, unicode and ESC escapes") {
    assert!(HEX_UNI == "A\u{e9}\e");
    assert!(HEX_UNI.len() == 4);
}

test("an escaped backslash before n is not a newline") {
    assert!(MIXED == "\t\"\\n");
    assert!(MIXED.len() == 4);
}
