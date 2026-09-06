// Local EarlyCSE (#317): stored Index / CastIntToFloat reuse must keep
// values; a store through the same index must kill the expr.

test("index reuse matches first load") {
    let a = [10, 20, 30, 40];
    let i = 2;
    let x = a[i];
    assert(x + a[i] + a[i] == 90)?;
}

test("index store kills reused slot") {
    let a = [1, 2, 3];
    let i = 1;
    let x = a[i];
    a[i] = x + 5;
    assert(a[i] == 7)?;
    assert(x == 2)?;
}

test("cast reuse stays IEEE-equal") {
    let i = 7;
    let xf = i as float;
    let s = xf + (i as float) + (i as float);
    assert(s == 21.0)?;
}

test("commutative add reuse") {
    let a = 11;
    let b = 31;
    let x = a + b;
    assert(x + (b + a) == 84)?;
}
