// Spreading an array literal, a fixed-array local or a tuple into a rest
// or fixed parameters type-checks (coil-lang#1153: the checker panicked).
use tests::assert;

fn total(int... xs) -> int {
    return len(xs);
}

fn sum(int... xs) -> int {
    let s = 0;
    for x in xs {
        s += x;
    }
    return s;
}

fn add(int a, int b) -> int {
    return a + b;
}

test("array literal spread into a rest") {
    let n = total(...[1, 2]);
    assert!(n == 2);
}

test("two array literal spreads into fixed parameters") {
    let a = add(...[1, 2]);
    let b = add(...[3, 4]);
    assert!(a == 3);
    assert!(b == 7);
}

test("fixed-array local spread into a rest") {
    let arr = [5, 6, 7];
    assert!(sum(...arr) == 18);
    assert!(sum(...arr, 1) == 19);
}

test("tuple spread into a rest") {
    assert!(sum(...(1, 2, 3)) == 6);
}

fn joined(string sep, string... parts) -> int {
    return len(parts);
}

test("tuple spread fills the fixed parameter, then the rest") {
    assert!(joined(...("+", "a")) == 1);
    assert!(joined(...("+", "a", "b", "c")) == 3);
}
