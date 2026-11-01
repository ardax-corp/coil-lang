// `len` counts UTF-8 bytes; `to_bytes` / `from_bytes` round-trip a string,
// and `from_bytes` gives `Err` on bytes that are not UTF-8.
use tests::assert;
use string::{to_bytes, from_bytes};

test("len counts bytes") {
    let s = "héllo";
    assert!(s.len() == 6);
    let e = "";
    assert!(e.len() == 0);
    let j = "日本";
    assert!(j.len() == 6);
}

test("to_bytes then from_bytes") {
    let s = "héllo 日本";
    let bs = to_bytes(s);
    assert!(bs.len() == s.len());
    assert!((bs[1] as int) == 195);
    let back = from_bytes(bs)?;
    assert!(back == s);
    assert!(to_bytes("").len() == 0);
    assert!(from_bytes(to_bytes(""))? == "");
}

test("from_bytes of a literal byte buffer") {
    assert!(from_bytes("ok")? == "ok");
}

test("invalid UTF-8 is an error") {
    let bad: Vec<byte> = Vec::new();
    bad.push(255);
    let rejected = match from_bytes(bad) {
        Result::Ok(_) => false,
        Result::Err(_) => true,
    };
    assert!(rejected);
    let cut = to_bytes("é");
    cut.pop();
    let rejected2 = match from_bytes(cut) {
        Result::Ok(_) => false,
        Result::Err(_) => true,
    };
    assert!(rejected2);
}
