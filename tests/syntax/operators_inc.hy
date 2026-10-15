// Prefix/postfix ++/-- and mixed operator edges.
use tests::assert;

test("postfix increment returns old") {
    let y = 0;
    let old = y++;
    assert!(old == 0);
    assert!(y == 1);
}

test("prefix increment returns new") {
    let z = 0;
    let n = ++z;
    assert!(n == 1);
    assert!(z == 1);
}

test("postfix decrement") {
    let x = 5;
    let old = x--;
    assert!(old == 5);
    assert!(x == 4);
}

test("prefix decrement") {
    let x = 5;
    let n = --x;
    assert!(n == 4);
    assert!(x == 4);
}

test("mixed bitwise and arithmetic") {
    // 5 & 3 = 1, 4 | 1 = 5, 1 + 5 = 6
    assert!(((5 & 3) + (4 | 1)) == 6);
    assert!(((8 >> 1) * 3) == 12);
    assert!((7 & 3) == 3);
}
