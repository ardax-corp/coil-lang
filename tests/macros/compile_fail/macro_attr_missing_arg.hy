// Expected: E0119 — `add_after` needs `by`.
use derive_macros::{add_after};

#[add_after]
fn f(int x) -> int {
    return x;
}
