// Fixed arrays are values: a copy or a whole-array store copies the
// elements, so later writes to one never show through the other.

fn sum3([int; 3] xs) -> int {
    return xs[0] + xs[1] + xs[2];
}

test("copies are independent") {
    let a = [1, 2, 3];
    let b = a;
    let c = b;
    b[0] = 10;
    c[1] = 20;
    assert(a[0] == 1 && a[1] == 2)?;
    assert(b[0] == 10 && b[1] == 2)?;
    assert(c[0] == 1 && c[1] == 20)?;
}

test("store a literal and another array") {
    let a = [1.5, 2.5];
    let b = [0.0, 0.0];
    b = a;
    a = [7.0, 8.0];
    a[1] = 9.0;
    assert(b[0] == 1.5 && b[1] == 2.5)?;
    assert(a[0] == 7.0 && a[1] == 9.0)?;
}

test("a copied array still passes whole") {
    let a = [4, 5, 6];
    let b = a;
    b[2] = 0;
    assert(sum3(a) == 15)?;
    assert(sum3(b) == 9)?;
}
