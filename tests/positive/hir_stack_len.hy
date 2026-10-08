// `len(a)` of a frame-slot array folds to its length and does not box it.
fn total() -> int {
    let a = [3, 4, 5];
    let s = 0;
    let i = 0;
    while i < len(a) {
        s = s + a[i];
        i = i + 1;
    }
    return s * len(a);
}

test("len of a stack array") {
    assert(total() == 36)?;
}
