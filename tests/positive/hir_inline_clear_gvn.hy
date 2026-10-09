// An inlined callee's stack array is cleared after the statement. The IL
// value numbering must see that clear as a write: before it did, `i + 1`
// was read back from the cleared element slot and the loop never ended.

fn take(int i) -> int {
    let xs = [i, i + 1];
    return xs[0] + xs[1];
}

fn pack(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        s = s + take(i);
        i = i + 1;
    }
    return s;
}

test("a cleared inline slot is not reused for a later value") {
    assert(pack(3) == 9)?;
    assert(pack(10) == 100)?;
}
