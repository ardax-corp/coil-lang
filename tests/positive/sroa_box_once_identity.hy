// Q1: two named escapes of a `[T; N]` local share one heap object.
fn observe([int; 3] a, [int; 3] b) -> int {
    a[0] = 99;
    return b[0];
}

fn bounce([int; 3] xs) -> [int; 3] {
    return xs;
}

class Holder {
    pub a: [int; 3]
}

test("two call-arg escapes are the same object") {
    let xs = [1, 2, 3];
    let a = bounce(xs);
    assert(observe(a, bounce(xs)) == 99)?;
}

test("return then call-arg reuse the boxed identity") {
    let xs = [4, 5, 6];
    let a = bounce(xs);
    assert(observe(a, a) == 99)?;
}

test("nested bounce args share one box") {
    let xs = [1, 2, 3];
    assert(observe(bounce(xs), bounce(xs)) == 99)?;
}

test("field store and later call-arg are the same object") {
    let xs = [1, 2, 3];
    let h = new Holder([0, 0, 0]);
    h.a = xs;
    assert(observe(h.a, xs) == 99)?;
}

test("host observe then two call-args share the box") {
    let xs = [1, 2, 3];
    let v = Vec::from(xs);
    assert(v[0] + v[1] + v[2] == 6)?;
    assert(observe(xs, xs) == 99)?;
}

test("mutation after escape is visible on the boxed identity") {
    let xs = [1, 2, 3];
    let a = bounce(xs);
    xs[0] = 77;
    assert(a[0] == 77)?;
    a[1] = 88;
    assert(xs[1] == 88)?;
}
