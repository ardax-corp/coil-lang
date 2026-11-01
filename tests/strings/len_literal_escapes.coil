// `len` of a string literal written in place counts its bytes after
// escapes are decoded, as through a local (#1174).
use tests::assert;

const QUOTE_LEN: int = "\"".len();

test("len of escaped literals in place") {
    let s = "\"";
    assert!("\"".len() == 1);
    assert!("\\".len() == 1);
    assert!("\x41".len() == 1);
    assert!("a\nb\t".len() == 4);
    assert!(s.len() == 1);
}

test("len of escaped literal in a const") {
    assert!(QUOTE_LEN == 1);
}
