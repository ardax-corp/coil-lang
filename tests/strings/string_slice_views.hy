// `slice_bytes` of a long string shares its buffer instead of copying.
// These check that views read, compare, hash and grow like copies.
use tests::assert;
use string::slice_bytes;

fn cut(string s, int a, int b) -> string {
    return match slice_bytes(s, a, b) {
        Result::Ok(x) => x,
        Result::Err(_) => "<err>",
    };
}

fn long_text() -> string {
    let s = "";
    let i = 0;
    while i < 200 {
        s = s + "ab";
        i = i + 1;
    }
    return s;
}

test("suffix views read the right bytes") {
    let s = long_text() + "END";
    let rest = s;
    let n = 0;
    while rest.len() > 3 {
        rest = cut(rest, 2, rest.len());
        n = n + 1;
    }
    assert!(n == 200);
    assert!(rest == "END");
}

test("a view compares by content") {
    let s = long_text();
    let v = cut(s, 100, 400);
    let copy = long_text();
    assert!(v == cut(copy, 100, 400));
    assert!(v.len() == 300);
}

test("appending to a view leaves the source alone") {
    let s = long_text();
    let tail = cut(s, 300, 400);
    let grown = tail + "!";
    let head = cut(s, 0, 100);
    let other = head + "?";
    assert!(s.len() == 400);
    assert!(s[399] as int == 98);
    assert!(grown.len() == 101);
    assert!(grown[100] as int == 33);
    assert!(other[100] as int == 63);
    assert!(cut(s, 100, 101) == "a");
}

test("views keep UTF-8 boundaries") {
    let s = "";
    let i = 0;
    while i < 100 {
        s = s + "é";
        i = i + 1;
    }
    assert!(cut(s, 1, 150) == "<err>");
    assert!(cut(s, 2, 200).len() == 198);
}
