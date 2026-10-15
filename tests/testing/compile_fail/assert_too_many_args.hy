// Expected: E0119 — `assert!` takes a condition and an optional message.
use tests::assert;

test("too many") {
    assert!(true, "a", "b");
}
