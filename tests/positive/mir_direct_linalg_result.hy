// A matrix product of tuples of tuples in direct MIR: the packed kernel's
// result is a boxed tuple of row tuples, one heap word.
use tests::assert;

fn product() -> int {
    let a = ((1, 2), (3, 4));
    let b = ((5, 6), (7, 8));
    let c = matmul(a, b);
    return c[0][0] * 1000000 + c[0][1] * 10000 + c[1][0] * 100 + c[1][1];
}

fn floats() -> float {
    let a = ((1.0, 2.0), (3.0, 4.0));
    let b = ((0.5, 0.0), (0.0, 2.0));
    let c = matmul(a, b);
    return c[0][0] + c[0][1] + c[1][0] + c[1][1];
}

test("matmul of tuples of tuples") {
    assert!(product() == 19224350);
    assert!(floats() == 14.5);
}
