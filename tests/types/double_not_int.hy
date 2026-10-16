// `!!x` on an int is a bool: whether `x` is non-zero (coil-lang#1149).
use tests::assert;

fn twice(int x) -> bool {
    return !!x;
}

test("double not of negative int literals") {
    let a = !(!-1);
    assert!(a);
    let b = !!-5;
    assert!(b);
    assert!(!(!(-1)));
}

test("double not of zero and one") {
    assert!(!(!1));
    assert!(!!0 == false);
}

test("double not of an int value") {
    assert!(twice(7));
    assert!(twice(-3));
    assert!(twice(0) == false);
    let x = 42;
    let y = !!x;
    assert!(y == true);
}

test("single not of int literals") {
    assert!(!0);
    assert!(!-1 == false);
    assert!(!7 == false);
}
