// A bare `... xs` rest parameter packs each call's own trailing arguments:
// calls with different numbers and types of arguments each get the shape
// they pass, and the body can index the pack and take its length
// (coil-lang#1281).
use tests::assert;

fn count(... xs) -> int {
    return xs.len();
}

fn first(... xs) {
    return xs[0];
}

fn second(int skip, ... xs) {
    return xs[1];
}

fn sum(... xs) -> int {
    let total = 0;
    let i = 0;
    while i < xs.len() {
        total += xs[i];
        i += 1;
    }
    return total;
}

fn never_called(... xs) -> int {
    return xs[9] * "only checked per call";
}

fn label(string head, ... xs) -> string {
    return head + xs[xs.len() - 1];
}

fn forward(... xs) -> int {
    return count(...xs) * 10;
}

test("each call packs its own shape") {
    assert!(count(1, 2) == 2);
    assert!(count(1, "a") == 2);
    assert!(count() == 0);
    assert!(count(1, "a", true) == 3);
}

test("a constant index takes that element's type") {
    assert!(first(7, "x") == 7);
    assert!(first("y", 2) == "y");
    assert!(second(0, 1.5, true));
    assert!(second(0, true, "z") == "z");
}

test("a runtime index walks same-typed elements") {
    assert!(sum(1, 2, 3) == 6);
    assert!(sum(5) == 5);
    assert!(sum(4, 4) == 8);
}

test("fixed parameters, named or not, come before the pack") {
    assert!(label("a", 1, "b") == "ab");
    assert!(label(head: "c", true, 2.5, "d") == "cd");
}

test("a pack spreads into another variadic call") {
    assert!(forward(1, "a", 2.5) == 30);
    assert!(forward() == 0);
}
