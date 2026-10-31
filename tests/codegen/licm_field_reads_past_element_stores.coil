// An element store cannot change a class field, nor a field store an
// element (#1559): loops and blocks that store one kind may reuse reads of
// the other. Stores of the same kind still kill reads through an alias.
use tests::assert;

class Cfg {
    pub scale: int,
    pub bias: int,
}

class Holder {
    pub items: Vec<int>,
}

fn apply(Cfg c, Vec<int> xs, Vec<int> out) {
    let n = xs.len();
    for i in 0..n {
        out[i] = xs[i] * c.scale + c.bias;
    }
}

// `d` may be `c`: the field store must keep `c.scale` in the loop.
fn bump(Cfg c, Cfg d, Vec<int> out) {
    for i in 0..out.len() {
        out[i] = c.scale;
        d.scale = d.scale + 1;
    }
}

// `out` may be `h.items`: the element store must keep `h.items[0]` in the
// loop, though the field `h.items` itself may move.
fn chain(Holder h, Vec<int> out) {
    for i in 0..out.len() {
        out[i] = h.items[0] + 1;
    }
}

// A field store cannot change an element.
fn sum_first(Cfg c, Vec<int> xs, int n) -> int {
    let s = 0;
    for i in 0..n {
        s = s + xs[0];
        c.bias = c.bias + i;
    }
    return s;
}

// One block: a field read survives an element store, and an element read
// a field store, but an element store through an alias kills an element read.
fn same_block(Cfg c, Vec<int> v, Vec<int> w) -> int {
    let a = c.scale;
    w[1] = 40;
    let b = c.scale;
    let x = v[0];
    c.bias = 7;
    let y = v[0];
    w[0] = 9;
    let z = v[0];
    return a + b + x + y + z * 1000;
}

test("element stores keep field reads correct") {
    let c = new Cfg(3, 1);
    let xs = Vec::from([1, 2, 3, 4]);
    let out = Vec::from([0, 0, 0, 0]);
    apply(c, xs, out);
    assert!(out[0] == 4);
    assert!(out[1] == 7);
    assert!(out[2] == 10);
    assert!(out[3] == 13);
}

test("element stores into the read array") {
    let c = new Cfg(2, 5);
    let xs = Vec::from([1, 2, 3]);
    apply(c, xs, xs);
    assert!(xs[0] == 7);
    assert!(xs[1] == 9);
    assert!(xs[2] == 11);
}

test("field store through an alias is seen") {
    let c = new Cfg(10, 0);
    let out = Vec::from([0, 0, 0]);
    bump(c, c, out);
    assert!(out[0] == 10);
    assert!(out[1] == 11);
    assert!(out[2] == 12);
    assert!(c.scale == 13);
    let d = new Cfg(0, 0);
    let other = Vec::from([0, 0]);
    bump(c, d, other);
    assert!(other[0] == 13);
    assert!(other[1] == 13);
    assert!(d.scale == 2);
}

test("element store through a field's array is seen") {
    let h = new Holder(Vec::from([5, 0, 0, 0]));
    chain(h, h.items);
    assert!(h.items[0] == 6);
    assert!(h.items[1] == 7);
    assert!(h.items[2] == 7);
    assert!(h.items[3] == 7);
    let out = Vec::from([0, 0]);
    chain(h, out);
    assert!(out[0] == 7);
    assert!(out[1] == 7);
}

test("field stores keep element reads correct") {
    let c = new Cfg(0, 0);
    let xs = Vec::from([4, 1]);
    assert!(sum_first(c, xs, 5) == 20);
    assert!(c.bias == 10);
    assert!(sum_first(c, xs, 0) == 0);
}

test("one block mixes field and element reads and stores") {
    let c = new Cfg(2, 0);
    let v = Vec::from([3, 0]);
    assert!(same_block(c, v, v) == 2 + 2 + 3 + 3 + 9000);
    assert!(v[1] == 40);
    assert!(c.bias == 7);
    let v2 = Vec::from([3, 0]);
    let w2 = Vec::from([0, 0]);
    assert!(same_block(c, v2, w2) == 2 + 2 + 3 + 3 + 3000);
}
