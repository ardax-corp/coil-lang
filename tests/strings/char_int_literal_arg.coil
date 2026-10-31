// An int literal passed to `char` coerces to its `byte` parameter, as for
// a user function's (#1175).
use tests::assert;

fn takes(byte b) -> byte {
    return b;
}

test("int literal to char and to a byte parameter") {
    let x = takes(97);
    assert!(char(x)? == "a");
    assert!(char(97)? == "a");
    assert!(char(122)? == "z");
}
