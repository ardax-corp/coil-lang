// A type alias declared in a function body or inner block shadows the outer
// alias only inside it; other functions and the rest of the body keep the
// outer meaning.
use tests::assert;

type Value = int;

type Local = int;

fn id(Value x) -> int {
    return x;
}

fn local() -> string {
    type Value = string;
    let s: Value = "ok";
    return s;
}

test("body alias shadows, other functions keep the outer alias") {
    assert!(local() == "ok");
    assert!(id(3) == 3);
    assert!((typeof id(3)) == "int");
}

test("an alias in an inner block does not leak out of it") {
    if true {
        type Local = string;
        let s: Local = "ok";
        assert!(s == "ok");
    }
    let n: Local = 1;
    assert!((typeof n) == "int");
}
