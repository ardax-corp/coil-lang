// Stack multi-slot locals: scalars, large N, nested rows on heap, pointer elems.

test("float and bool locals") {
    let f = [1.0, 2.5, 3.0];
    assert(f[0] == 1.0)?;
    f[1] = 9.5;
    assert(f[1] == 9.5)?;

    let b = [true, false, true, false];
    assert(b[0])?;
    assert(!b[1])?;
    b[1] = true;
    assert(b[1])?;
}

test("byte local") {
    let buf: [byte; 4] = [1, 2, 3, 4];
    assert(buf[2] == (3 as byte))?;
    buf[2] = 30 as byte;
    assert(buf[2] == (30 as byte))?;
}

test("large n int local") {
    let a = [1, 2, 3, 4, 5, 6];
    assert(a[0] == 1)?;
    assert(a[5] == 6)?;
    a[4] = 40;
    assert(a[4] == 40)?;
    let copy = a;
    assert(copy[4] == 40)?;
    copy[0] = 99;
    assert(a[0] == 1)?;
}

test("nested rows are heap elems") {
    let m = [[1, 2], [3, 4], [5, 6]];
    assert(m[0][0] == 1)?;
    assert(m[2][1] == 6)?;
    m[1][0] = 30;
    assert(m[1][0] == 30)?;
}

test("string pointer elems") {
    let s = ["a", "b", "c"];
    assert(s[1] == "b")?;
    s[1] = "x";
    assert(s[1] == "x")?;
}

test("unproven index load and store") {
    let xs = [10, 20, 30];
    let k = 1;
    assert(xs[k] == 20)?;
    xs[k] = 7;
    assert(xs[0] + xs[1] + xs[2] == 47)?;
    let m = 2;
    xs[k % m] = 3;
    assert(xs[1] == 3)?;
}

test("observed zip stays heap") {
    let x = 1;
    let a = [x, x + 1] + [3, 4];
    assert(a[0] == 4)?;
    assert(a[1] == 6)?;
    let v = 9;
    a[0] = v;
    assert(a[0] + a[1] == 15)?;
    let xs = [x, x + 1];
    let ys = [3, 4];
    let z = xs + ys;
    assert(z[0] + z[1] == 10)?;
    let b = [x, x + 1] + 3;
    assert(b[0] == 4)?;
    assert(b[1] == 5)?;
    let c = [x, x + 1] ** 3;
    assert(c[0] == 1)?;
    assert(c[1] == 8)?;
}

test("whole array assign copies slots") {
    let a = [1, 2, 3, 4];
    let b = [0, 0, 0, 0];
    b = a;
    assert(b[0] == 1)?;
    assert(b[3] == 4)?;
    b[0] = 9;
    assert(a[0] == 1)?;
    a = [7, 8, 9, 10];
    assert(a[2] == 9)?;
}
