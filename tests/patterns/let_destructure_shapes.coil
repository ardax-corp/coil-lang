// `let` and `for` destructuring: tuple patterns with `_`, nested tuples,
// record patterns that rename a field, and tuples taken apart per loop
// iteration.
use tests::assert;

fn divmod(int a, int b) -> (int, int) {
    return (a / b, a % b);
}

test("a tuple from a call") {
    let (q, r) = divmod(17, 5);
    assert!(q == 3);
    assert!(r == 2);
}

test("wildcards skip items") {
    let (_, r) = divmod(9, 4);
    assert!(r == 1);
    let (a, _, c) = (1, "unused", 3);
    assert!(a + c == 4);
}

test("nested tuples") {
    let ((a, b), (c, (d, e))) = ((1, 2), (3, (4, 5)));
    assert!(a * 10000 + b * 1000 + c * 100 + d * 10 + e == 12345);
}

test("record patterns with renames") {
    let p = { x: 3, y: 4 };
    let { x: px, y } = p;
    assert!(px == 3);
    assert!(y == 4);
    let { y: second, x: first } = p;
    assert!(first * 10 + second == 34);
}

test("a tuple pattern in a for loop") {
    let pairs = [(1, "a"), (2, "b"), (3, "c")];
    let n = 0;
    let s = "";
    for (k, v) in pairs {
        n = n + k;
        s = s + v;
    }
    assert!(n == 6);
    assert!(s == "abc");
}
