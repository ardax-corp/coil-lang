// Each `test("…")` case runs on its own: a case's name may hold spaces or
// punctuation, cases share top-level helpers, and a static written in one
// case starts from its initializer in the next.
use tests::assert;

static let seen: int = 0;

fn twice(int n) -> int {
    return n * 2;
}

test("addition works") {
    assert!(1 + 1 == 2);
    assert!(twice(21) == 42);
}

test("multiply_works") {
    assert!(3 * 4 == 12);
}

test("names may hold punctuation: (a, b) -> c!") {
    assert!(twice(0) == 0);
}

test("first case writes the static") {
    assert!(seen == 0);
    seen = 7;
    assert!(seen == 7);
}

test("second case sees the static reset") {
    assert!(seen == 0);
    seen = 9;
}
