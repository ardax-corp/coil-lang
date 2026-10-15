// A type-only `let x: T;` declares a local assigned later (coil-lang#1145).
use tests::assert;

fn pick(bool c) -> int {
    let x: int;
    if c {
        x = 1;
    } else {
        x = 2;
    }
    return x;
}

test("type-only let assigned later") {
    let x: int;
    x = 3;
    assert!(x == 3);
}

test("type-only let assigned in both branches") {
    assert!(pick(true) == 1);
    assert!(pick(false) == 2);
}

test("type-only let never used") {
    let unused: int;
    assert!(true);
}

test("type-only float, bool and byte lets") {
    let f: float;
    f = 1.5;
    let b: bool;
    b = true;
    let y: byte;
    y = 7;
    assert!(f == 1.5);
    assert!(b);
    assert!(y == 7);
}

test("type-only let assigned in a loop") {
    let last: int;
    for i in 0..4 {
        last = i * 2;
    }
    assert!(last == 6);
}
