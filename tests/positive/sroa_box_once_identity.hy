// Q1: two named escapes of a `[T; N]` local share one heap object.
fn observe([int; 3] a, [int; 3] b) -> int {
    a[0] = 99;
    return b[0];
}

test("two call-arg escapes are the same object") {
    let xs = [1, 2, 3];
    assert(observe(xs, xs) == 99)?;
}

fn bounce([int; 3] xs) -> [int; 3] {
    return xs;
}

test("return then call-arg reuse the boxed identity") {
    let xs = [4, 5, 6];
    let a = bounce(xs);
    assert(observe(a, a) == 99)?;
}
