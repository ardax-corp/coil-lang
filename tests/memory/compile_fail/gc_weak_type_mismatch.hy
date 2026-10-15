// Expected: E0102 — `weak("x")` is a `Weak<string>`, not a `Weak<int>`.
use gc::{weak, Weak};

fn main() {
    let w: Weak<int> = weak("x");
}
