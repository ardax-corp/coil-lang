// Matches the checker accepts: every variant listed (unit variants may be
// written `Option::None()`), a `default` arm, nested `_` inside a payload,
// and nested constructor patterns on enums declared after their use.
use tests::assert;

fn wrapped() -> int {
    let x = Wrap::W(Inner::I(7));
    return match x {
        Wrap::W(Inner::I(v)) => v,
    };
}

enum Inner {
    I(int),
}

enum Wrap {
    W(Inner),
}

test("all variants listed") {
    let x = Option::Some(1);
    let r = match x {
        Option::None => 0,
        Option::Some(v) => v,
    };
    assert!(r == 1);
}

test("a default arm is exhaustive") {
    let x = Option::Some(1);
    let r = match x {
        default => 0,
    };
    assert!(r == 0);
}

test("nested underscore in a payload is fine") {
    let x = Option::Some(1);
    let a = match x {
        Option::None => 0,
        Option::Some(_) => 1,
    };
    assert!(a == 1);
    let y: Result<int, string> = Result::Ok(1);
    let b = match y {
        Result::Ok(_) => 0,
        Result::Err(_) => 1,
    };
    assert!(b == 0);
}

test("nested constructor pattern on enums declared later") {
    assert!(wrapped() == 7);
}

test("a constructor of a prelude enum needs no declaration") {
    let x = Option::Some(1);
    assert!((typeof x) == "prelude::Option<int>");
}

enum InnerAB {
    A,
    B,
}

enum OuterV {
    V(InnerAB, InnerAB),
}

fn classify(OuterV p) -> int {
    return match p {
        OuterV::V(InnerAB::A, InnerAB::B) => 1,
        OuterV::V(InnerAB::A, InnerAB::A) => 2,
        OuterV::V(_, _) => 0,
    };
}

test("arms that differ only in the second payload are all reachable") {
    assert!(classify(OuterV::V(InnerAB::A, InnerAB::B)) == 1);
    assert!(classify(OuterV::V(InnerAB::A, InnerAB::A)) == 2);
    assert!(classify(OuterV::V(InnerAB::B, InnerAB::A)) == 0);
}
