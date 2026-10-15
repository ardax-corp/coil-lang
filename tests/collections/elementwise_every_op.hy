// Element-wise operators on arrays, tuples and Vec: every arithmetic operator
// zipped, scalar broadcast on either side, unary `-`, compound assignment,
// long arrays and wrapping byte elements.
use tests::assert;

test("array zip every operator") {
    let a = [6, 8, 9];
    let b = [3, 2, 4];
    assert!(a + b == [9, 10, 13]);
    assert!(a - b == [3, 6, 5]);
    assert!(a * b == [18, 16, 36]);
    assert!(a / b == [2, 4, 2]);
    assert!(a % b == [0, 0, 1]);
    assert!(b ** [2, 3, 1] == [9, 8, 4]);
    assert!(-a == [-6, -8, -9]);
}

test("broadcast both sides") {
    assert!([1, 2] + 3 == [4, 5]);
    assert!(10 - [1, 2] == [9, 8]);
    assert!(2.0 * (1.5, 2.0) == (3.0, 4.0));
    assert!((7, 9) % 4 == (3, 1));
}

test("tuple zip and nested") {
    assert!((1, 2, 3) * (4, 5, 6) == (4, 10, 18));
    assert!(-(1.0, -2.0) == (-1.0, 2.0));
}

test("compound assign") {
    let a = [1, 2, 3];
    a += [1, 1, 1];
    a *= 2;
    assert!(a == [4, 6, 8]);
    let t = (1, 2);
    t += (10, 20);
    assert!(t == (11, 22));
}

test("vec broadcast and negate") {
    let v = Vec::from([1, 2, 3]);
    let w = 2 * v;
    let n = -v;
    assert!(w == Vec::from([2, 4, 6]));
    assert!(n[2] == -3);
    assert!(v[0] == 1);
}

test("long arrays") {
    let a = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    let b = a * a;
    assert!(b[9] == 100 && b[0] == 1);
    let c = a + 1;
    assert!(c[9] == 11);
}

test("byte arrays wrap") {
    let a: [byte; 2] = [250, 1];
    let b: [byte; 2] = [10, 2];
    let c = a + b;
    assert!((c[0] as int) == 4);
    assert!((c[1] as int) == 3);
}
