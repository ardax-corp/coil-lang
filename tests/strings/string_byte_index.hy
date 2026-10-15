// `s[i]` on a string reads the byte at byte offset `i` in direct MIR, as
// `string_byte_at` answers it.
use tests::assert;

fn digits(string s) -> int {
    let n = 0;
    let i = 0;
    while i < len(s) {
        let c = s[i] as int;
        if c >= 48 && c <= 57 {
            n = n * 10 + (c - 48);
        }
        i = i + 1;
    }
    return n;
}

fn last(string s) -> int {
    return s[len(s) - 1] as int;
}

test("string bytes by index") {
    assert!(digits("a1b2c3") == 123);
    assert!(digits("") == 0);
    assert!(last("xyz") == 122);
    assert!(last("é") == 169);
}
