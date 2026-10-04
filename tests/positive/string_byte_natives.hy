use string::{byte_at, slice_bytes, find_from, rfind, match_at};

fn sliced(string s, int start, int end) -> string {
    return match slice_bytes(s, start, end) {
        Result::Ok(x) => x,
        Result::Err(_) => "<err>",
    };
}

test("byte_at reads bytes and reports out of range") {
    assert(byte_at("abc", 0) == 97)?;
    assert(byte_at("abc", 2) == 99)?;
    assert(byte_at("abc", 3) == 0 - 1)?;
    assert(byte_at("abc", 0 - 1) == 0 - 1)?;
}

test("slice_bytes clamps and rejects split code points") {
    assert(sliced("hello", 1, 3) == "el")?;
    assert(sliced("hello", 0 - 2, 99) == "hello")?;
    assert(sliced("hello", 4, 2) == "")?;
    assert(sliced("hé!", 1, 3) == "é")?;
    assert(sliced("hé!", 2, 4) == "<err>")?;
}

test("find_from rfind and match_at") {
    assert(find_from("a,b,c", ",", 0) == 1)?;
    assert(find_from("a,b,c", ",", 2) == 3)?;
    assert(find_from("a,b,c", ",", 4) == 0 - 1)?;
    assert(find_from("abc", "", 9) == 3)?;
    assert(rfind("a,b,c", ",") == 3)?;
    assert(rfind("abc", "x") == 0 - 1)?;
    assert(match_at("hello", "he", 0))?;
    assert(match_at("hello", "lo", 3))?;
    assert(!match_at("hello", "lo", 4))?;
    assert(!match_at("hello", "he", 0 - 1))?;
}

test("qualified string natives on built strings") {
    let s = "";
    let i = 0;
    while i < 50 {
        s = s + "ab";
        i = i + 1;
    }
    assert(string::byte_at(s, 99) == 98)?;
    assert(string::find_from(s, "ba", 10) == 11)?;
    assert(string::rfind(s, "ab") == 98)?;
}
