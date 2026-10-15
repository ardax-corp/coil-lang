// `ord` maps a one-character string (codepoint <= 255) to its byte and
// `char` maps a byte back; other inputs give `Err`. One-byte literals
// coerce to `byte` in annotations.
use tests::assert;

fn is_err(Result<byte, string> r) -> bool {
    return match r {
        Result::Ok(_) => false,
        Result::Err(_) => true,
    };
}

test("ord of one character") {
    assert!((ord("A")? as int) == 65);
    assert!((ord("\n")? as int) == 10);
    assert!((ord("é")? as int) == 233);
}

test("ord rejects empty and longer strings") {
    assert!(is_err(ord("ab")));
    assert!(is_err(ord("")));
    assert!(is_err(ord("日")));
}

test("char inverts ord for ASCII") {
    let a: byte = 97;
    assert!(char(a)? == "a");
    let z = ord("z")?;
    assert!(char(z)? == "z");
}

test("one-byte literals coerce to byte") {
    let c: byte = "A";
    assert!((c as int) == 65);
    let s: byte = "/";
    assert!(s == "/");
}
