// With the builtin `len(x)` gone, `len` is an ordinary name: a program may
// declare its own free `len`, and `.len()` still reads the length.
use tests::assert;

fn len(Vec<int> v, int extra) -> int {
    return v.len() + extra;
}

test("a user free function named len") {
    let v: Vec<int> = [1, 2, 3];
    assert!(len(v, 10) == 13);
    assert!(v.len() == 3);
}
