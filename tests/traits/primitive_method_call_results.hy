// #1030: calling a primitive's operator or `into` method directly
// (`a.add(4)`, `3.into()`) gives the number, not a boxed copy of it, and a
// generic call through the trait still works.
use tests::assert;

fn plus<T: Add>(T a, T b) -> T {
    return a + b;
}

fn negate<T: Neg>(T a) -> T {
    return -a;
}

test("int operator methods") {
    let a = 3;
    let x = a.add(4);
    assert!(x + 1 == 8);
    assert!(a.sub(5) == -2);
    assert!(a.mul(4) == 12);
    assert!(a.mul(4).add(1) == 13);
    assert!(10.div(3) == 3);
    assert!(10.rem(3) == 1);
    assert!(2.pow(10) == 1024);
    assert!(a.neg() == -3);
}

test("int bitwise methods") {
    let a = 6;
    assert!(a.shl(2) == 24);
    assert!(a.shr(1) == 3);
    assert!(a.bitand(3) == 2);
    assert!(a.bitor(1) == 7);
    assert!(a.bitxor(5) == 3);
    assert!(a.bitnot() == ~6);
}

test("float operator methods") {
    let f = 1.5;
    let g = f.add(2.0);
    assert!(g * 2.0 == 7.0);
    assert!(f.sub(0.5) == 1.0);
    assert!(f.mul(2.0) == 3.0);
    assert!(f.div(0.5) == 3.0);
    assert!(f.neg() < 0.0);
}

test("into a different primitive") {
    let f: float = 3.into();
    assert!(f == 3.0);
    assert!(f + 0.5 == 3.5);
    let n: int = 2.75.into();
    assert!(n == 2);
    let b: bool = 1.into();
    assert!(b);
}

test("generic calls through the trait") {
    assert!(plus(2, 3) == 5);
    assert!(plus(1.5, 1.0) == 2.5);
    assert!(negate(4) == -4);
}
