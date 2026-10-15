// `len` of a fixed array is its own length even when a later test case
// binds the same local name to an array of another length.
use tests::assert;

test("len of literal") {
    let a = [1, 2, 3, 4];
    assert!(len(a) == 4);
}

test("nested array index") {
    let a = [[1, 2], [3, 4]];
    assert!(a[0][1] == 2);
    assert!(len(a) == 2);
}
