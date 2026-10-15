// Expected: E0119 — a file with `test("…")` cases must not define `main`.
use tests::assert;

test("with main") {
    assert!(true);
}

fn main() {}
