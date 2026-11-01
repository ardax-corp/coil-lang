// Element-wise array results used as values: a zip read back, a zip of two
// locals, a zip stored into, a scalar broadcast, `**`, and a zip returned.
use tests::assert;

fn zip_sum(int x) -> int {
    let a = [x, x + 1] + [3, 4];
    return a[0] + a[1];
}

fn from_slots(int x) -> int {
    let xs = [x, x + 1];
    let ys = [3, 4];
    let a = xs + ys;
    return a[0] + a[1];
}

fn poke(int x, int v) -> int {
    let a = [x, x + 1] + [3, 4];
    a[0] = v;
    return a[0] + a[1];
}

fn broadcast() -> int {
    let b = [1, 2] + 3;
    return b[0] * 10 + b[1];
}

fn pow2() -> int {
    let c = [1, 2] ** 3;
    return c[0] * 10 + c[1];
}

fn escape() -> [int; 2] {
    return [1, 2] + [3, 4];
}

test("element-wise results read, stored and returned") {
    assert!(zip_sum(1) == 10);
    assert!(from_slots(1) == 10);
    assert!(poke(1, 9) == 15);
    assert!(broadcast() == 45);
    assert!(pow2() == 18);
    let e = escape();
    assert!(e[0] * 10 + e[1] == 46);
}
