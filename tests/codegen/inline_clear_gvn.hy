// A small function that builds a local array, called in a counted loop,
// returns the right value each call and the loop ends (the array's elements
// are not confused with values computed from the loop counter).
use tests::assert;

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
    assert!(pack(3) == 9);
    assert!(pack(10) == 100);
}
