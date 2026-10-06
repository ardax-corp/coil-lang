// Element-wise array and tuple operators lower from HIR: literal items and
// stack-array slots are read in place, other operands from temps, a static
// shape of at least 8 takes the packed kernel and a dynamic length loops.
fn first([int] xs) -> int {
    return xs[0];
}

fn neg_dyn([int] xs) -> [int] {
    return -xs;
}

fn scale_dyn([int] xs, int k) -> [int] {
    return k * xs;
}

fn zip_slots(int x) -> int {
    let a = [x, x + 1];
    let b = [10, 20];
    a[0] = 5;
    let c = a + b;
    return c[0] * 100 + c[1];
}

fn zip_boxed(int x) -> int {
    let a = [x, x + 1];
    let seen = first(a);
    let c = a - [1, 1];
    return seen * 100 + c[0] * 10 + c[1];
}

fn packed(int k) -> int {
    let a = [1, 2, 3, 4, 5, 6, 7, 8];
    let p = a + k;
    let n = -a;
    return p[7] * 100 + n[0];
}

test("zip, broadcast and negate on arrays") {
    assert(zip_slots(1) == 1522)?;
    assert(zip_boxed(3) == 323)?;
    assert(packed(2) == 999)?;
    let m = [7, 8] % 3;
    assert(m[0] == 1 && m[1] == 2)?;
    let p = 2 ** [1, 3];
    assert(p[0] == 2 && p[1] == 8)?;
    let f = [1.5, 2.0] * [2.0, 0.5];
    assert(f[0] == 3.0 && f[1] == 1.0)?;
}

test("tuples and dynamic arrays") {
    let t = (1, 2) + (10, 20);
    assert(t[0] == 11 && t[1] == 22)?;
    let s = 3 - (1, 2);
    assert(s[0] == 2 && s[1] == 1)?;
    let n = -(4, 5);
    assert(n[0] == -4 && n[1] == -5)?;
    let d = neg_dyn([1, 2, 3]);
    assert(len(d) == 3 && d[2] == -3)?;
    let k = scale_dyn([1, 2, 3], 4);
    assert(len(k) == 3 && k[0] == 4 && k[2] == 12)?;
}
