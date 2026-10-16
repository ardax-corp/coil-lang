// `len` of a string literal written in place counts its bytes after
// escapes are decoded, as through a local (#1174).
use tests::assert;

const QUOTE_LEN: int = len("\"");

test("len of escaped literals in place") {
    let s = "\"";
    assert!(len("\"") == 1);
    assert!(len("\\") == 1);
    assert!(len("\x41") == 1);
    assert!(len("a\nb\t") == 4);
    assert!(len(s) == 1);
}

test("len of escaped literal in a const") {
    assert!(QUOTE_LEN == 1);
}
