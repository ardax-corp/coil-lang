use tests::assert;
use string::slice_bytes;

fn sliced(string s, int start, int end) -> string {
    return match slice_bytes(s, start, end) {
        Result::Ok(x) => x,
        Result::Err(_) => "<err>",
    };
}

test("slice_bytes clamps and rejects split code points") {
    assert!(sliced("hello", 1, 3) == "el");
    assert!(sliced("hello", 0 - 2, 99) == "hello");
    assert!(sliced("hello", 4, 2) == "");
    assert!(sliced("hé!", 1, 3) == "é");
    assert!(sliced("hé!", 2, 4) == "<err>");
}
