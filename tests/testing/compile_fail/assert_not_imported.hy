// Expected: E0119 — `assert!` comes from `use tests::assert;`; it is not in the
// prelude.
test("no import") {
    assert!(true);
}
