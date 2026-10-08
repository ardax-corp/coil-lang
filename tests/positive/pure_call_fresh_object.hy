// coil-lang#790: CSE and LICM must not merge or hoist calls to a pure fn
// that returns a fresh array: each call gives its own object.
fn mk() -> [int] {
    return [0, 0];
}

test("two calls give two arrays") {
    let a = mk();
    let b = mk();
    a[0] = 1;
    assert(b[0] == 0)?;
}

test("a call in a loop gives a fresh array each time") {
    let total = 0;
    let i = 0;
    while i < 3 {
        let a = mk();
        total = total + a[0];
        a[0] = 5;
        i = i + 1;
    }
    assert(total == 0)?;
}
