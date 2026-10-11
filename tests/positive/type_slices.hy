// There is no `[T]` (#890): `Vec<T>` is the one unknown-length array, and
// an array or string literal builds one where it is expected.
fn head(Vec<int> xs) -> int {
    return xs[0];
}

fn empty_row() -> Vec<int> {
    let xs: Vec<int> = [];
    return xs;
}

test("Vec param takes an array literal and indexes at runtime") {
    let n = head([7, 8, 9]);
    assert(n == 7)?;
}

test("Vec<byte> from a string literal") {
    let buf: Vec<byte> = "ok";
    assert(len(buf) == 2)?;
}

test("fixed array keeps static length") {
    let xs: [int; 2] = [1, 2];
    assert(len(xs) == 2)?;
}

test("fn return of an empty Vec") {
    let row = empty_row();
    assert(len(row) == 0)?;
}
