// A generic numeric body cloned at a tuple or array type does element-wise
// arithmetic; the shape comes from the clone's operand types.

fn add<T: Num>(T a, T b) -> T {
    return a + b;
}

fn neg<T: Num>(T a) -> T {
    return -a;
}

fn scale<T: Num>((T, T) v, T s) -> (T, T) {
    return v * s;
}

test("tuple add in a clone") {
    let t = add((2, 3), (3, 2));
    assert(t[0] == 5 && t[1] == 5)?;
}

test("array add and negation") {
    let a = add([1, 2, 3], [10, 20, 30]);
    assert(a[0] + a[1] + a[2] == 66)?;
    let n = neg((1.5, -2.0));
    assert(n[0] == -1.5 && n[1] == 2.0)?;
}

test("broadcast") {
    let s = scale((1, 2), 3);
    assert(s[0] == 3 && s[1] == 6)?;
}
