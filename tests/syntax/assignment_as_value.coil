// An assignment or compound assignment used as a value evaluates to the
// value it stored (coil-lang#1144): plain, chained, compound, and on a
// field or an element.
use tests::assert;

class D {
    pub a: int,
}

test("plain assignment as a let value") {
    let x = 0;
    let y = x = 5;
    assert!(x == 5);
    assert!(y == 5);
}

test("chained assignment") {
    let a = 0;
    let b = 0;
    a = b = 7;
    assert!(a == 7);
    assert!(b == 7);
}

test("compound assignment as a value") {
    let x = 1;
    let y = x += 5;
    assert!(x == 6);
    assert!(y == 6);
    let z = 1;
    assert!((z -= 3) == -2);
    assert!(z == -2);
}

test("chained compound assignment") {
    let a = 1;
    let b = 2;
    a += b += 3;
    assert!(b == 5);
    assert!(a == 6);
}

test("field assignment as a value") {
    let d = new D(0);
    let v = d.a = 9;
    assert!(v == 9);
    assert!(d.a == 9);
}

test("element compound assignment as a value") {
    let xs = [1, 2, 3];
    let w = xs[1] += 10;
    assert!(w == 12);
    assert!(xs[1] == 12);
}

test("float and string assignment as a value") {
    let f = 0.5;
    let g = f *= 4.0;
    assert!(g == 2.0);
    let s = "a";
    let t = s = "bc";
    assert!(t == "bc");
    assert!(s == "bc");
}

fn step(Vec<int> log, int tag, int out) -> int {
    log.push(tag);
    return out;
}

test("call index assignment as a value runs the value first") {
    let log: Vec<int> = Vec::new();
    let a = Vec::from([0, 0, 0]);
    let v = a[step(log, 2, 2)] = step(log, 1, 7);
    assert!(v == 7);
    assert!(a[2] == 7);
    assert!(log.len() == 2);
    assert!(log[0] == 1);
    assert!(log[1] == 2);
}
