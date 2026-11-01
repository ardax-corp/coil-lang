use tests::assert;
use string::{format, to_bytes};

fn find_opt(Vec<byte> hay, Vec<byte> needle) -> Option<int> {
    let i = 0;
    while i < hay.len() {
        if hay[i] == needle[0] {
            return Option::Some(i);
        }
        i = i + 1;
    }
    return Option::None;
}

test("signed int compares") {
    let x = -5;
    assert!(x < 0);
    assert!(x <= -1);
    assert!(!(x > 0));
    assert!(!(x >= 0));
    assert!(x == -5);
}

test("match Option payload survives arm temps") {
    let found = match find_opt(to_bytes("abcdef"), to_bytes("c")) {
        Option::Some(i) => i,
        Option::None => -1,
    };
    assert!(found == 2);
    // HostInvoke temps in the arm must not clobber binding `i`.
    let labeled = match find_opt(to_bytes("abcdef"), to_bytes("c")) {
        Option::Some(i) => format("%i", i),
        Option::None => "none",
    };
    assert!(labeled == "2");
}

test("assign cast to byte") {
    let c: byte = 65;
    let m = 97;
    c = m as byte;
    assert!((c as int) == 97);
}
