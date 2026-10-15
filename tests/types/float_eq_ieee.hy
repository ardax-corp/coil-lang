// `==` / `!=` on floats follow IEEE 754 (#1077): NaN is unequal to
// everything, itself included, and `0.0 == -0.0`. Floats inside arrays and
// tuples compare the same way.
use tests::assert;

fn nan() -> float {
    let z = 0.0;
    return z / z;
}

fn neg_zero() -> float {
    return -0.0;
}

fn eq(float a, float b) -> bool {
    return a == b;
}

fn ne(float a, float b) -> bool {
    return a != b;
}

test("NaN is not equal to itself") {
    let n = nan();
    assert!(!(n == n));
    assert!(n != n);
    let m = n;
    assert!(!(n == m));
}

test("NaN through a function call") {
    let n = nan();
    assert!(!eq(n, n));
    assert!(ne(n, n));
    assert!(!eq(n, 1.0));
    assert!(ne(1.0, n));
}

test("the x != x NaN check") {
    let xs = [1.0, nan(), 3.0];
    let count = 0;
    for x in xs {
        if x != x {
            count += 1;
        }
    }
    assert!(count == 1);
}

test("positive and negative zero are equal") {
    let z = neg_zero();
    assert!(z == 0.0);
    assert!(eq(0.0, z));
    assert!(!ne(z, 0.0));
}

test("ordinary floats still compare") {
    assert!(eq(1.5, 1.5));
    assert!(ne(1.5, 2.5));
    assert!(0.1 + 0.2 != 0.3);
}

test("floats inside aggregates") {
    let n = nan();
    assert!(!([n] == [n]));
    assert!([n] != [n]);
    assert!([0.0] == [neg_zero()]);
    assert!(!((n, 1) == (n, 1)));
    assert!((1.0, 2) == (1.0, 2));
}
