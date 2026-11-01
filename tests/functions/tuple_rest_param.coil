// A bare `... name` rest parameter packs the trailing arguments into a
// tuple (coil-lang#1150).
use tests::assert;

fn zero(... xs) -> int {
    return 0;
}

fn pair_sum(... xs) -> int {
    let (a, b) = xs;
    return a + b;
}

fn zero_mixed(... xs) -> int {
    return 1;
}

fn zero_empty(... xs) -> int {
    return 2;
}

fn first_of(... xs) -> int {
    let (n, s, b) = xs;
    return n;
}

fn label(string head, ... xs) -> string {
    let (n, s) = xs;
    return head + s;
}

fn sub3(int a, int b, int c) -> int {
    return a - b - c;
}

fn forward(... xs) -> int {
    return sub3(...xs);
}

test("unused tuple pack") {
    assert!(zero(1, 2) == 0);
    assert!(zero(3, 4) == 0);
    assert!(zero_mixed(1, "a") == 1);
    assert!(zero_empty() == 2);
}

test("destructured tuple pack") {
    assert!(pair_sum(1, 2) == 3);
    assert!(pair_sum(40, 2) == 42);
}

test("mixed-type tuple pack") {
    assert!(first_of(7, "x", true) == 7);
}

test("fixed parameter before a tuple pack") {
    assert!(label("a", 1, "b") == "ab");
}

test("tuple pack spread into fixed parameters") {
    assert!(forward(10, 3, 2) == 5);
}
