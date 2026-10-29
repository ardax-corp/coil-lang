// Assigning a fixed array or a `Vec` to another local copies it (on write);
// `let w = mut v` borrows it, and a parameter a function writes is written
// in place for the caller.
use tests::assert;

fn zero_first_vec(Vec<int> v) {
    v[0] = 0;
}

test("array assignment copies") {
    let a = [1, 2, 3];
    let b = a;
    b[0] = 9;
    assert!(a[0] == 1);
    assert!(b[0] == 9);
}

test("Vec assignment copies on write") {
    let v = Vec::from([1, 2, 3]);
    let w = v;
    w[0] = 9;
    assert!(v[0] == 1);
    w.push(4);
    assert!(v.len() == 3);
    assert!(w.len() == 4 && w[0] == 9);
}

test("a borrowed Vec is one object") {
    let v = Vec::from([1, 2, 3]);
    let w = mut v;
    w[0] = 9;
    assert!(v[0] == 9);
    w.push(4);
    assert!(v.len() == 4);
}

test("a Vec parameter writes through") {
    let v = Vec::from([5, 6]);
    zero_first_vec(v);
    assert!(v[0] == 0);
}
