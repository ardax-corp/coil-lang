// Results of calls on both sides of a `+` stay intact across the second
// call: two loop helpers, and a recursive call whose argument is a call.
use tests::assert;

fn left(int n) -> int {
    let s = 0;
    let i = 0;
    while i < n {
        s = s + i;
        i = i + 1;
    }
    return s;
}

fn right(int n) -> int {
    let s = 1;
    let i = 0;
    while i < n {
        s = s + i;
        i = i + 1;
    }
    return s;
}

#[max_depth(16)]
fn leaf(int n) -> int {
    if n <= 0 {
        return 1;
    }
    return n + leaf(n - 1);
}

test("two pure helper results summed") {
    // left(5) = 10, right(4) = 7
    assert!(left(5) + right(4) == 17);
}

test("a nested call argument") {
    // leaf(leaf(2)) = leaf(4) = 11, leaf(3) = 7
    assert!(leaf(leaf(2)) + leaf(3) == 18);
}
