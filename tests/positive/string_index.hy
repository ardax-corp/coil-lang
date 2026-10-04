// `s[i]` reads the byte at byte offset `i`, like `to_bytes(s)[i]`.
use string::to_bytes;

fn count(string s, byte b) -> int {
    let n = 0;
    let i = 0;
    while i < len(s) {
        if s[i] == b {
            n = n + 1;
        }
        i = i + 1;
    }
    return n;
}

fn first(string s) -> string {
    return s;
}

test("index reads bytes") {
    let s = "a/b/c";
    assert(s[0] == "a")?;
    assert(s[1] == "/")?;
    assert((s[4] as int) == 99)?;
    assert(count(s, "/") == 2)?;
}

test("index is by byte, not by char") {
    let s = "héllo";
    assert((s[1] as int) == 195)?;
    assert((s[2] as int) == 169)?;
    assert(s[3] == "l")?;
    assert(len(s) == 6)?;
}

test("index matches to_bytes") {
    let s = "coil";
    let bs = to_bytes(s);
    let i = 0;
    while i < len(s) {
        assert(s[i] == bs[i])?;
        i = i + 1;
    }
}

test("index operands keep their order") {
    let s = "az";
    assert((s[1] as int) - (s[0] as int) == 25)?;
    assert((first(s)[1] as int) - (first(s)[0] as int) == 25)?;
}
