// `a.len()` of a frame-slot array folds to its length and does not box it.
use tests::assert;

fn total() -> int {
    let a = [3, 4, 5];
    let s = 0;
    let i = 0;
    while i < a.len() {
        s = s + a[i];
        i = i + 1;
    }
    return s * a.len();
}

test("len of a stack array") {
    assert!(total() == 36);
}
