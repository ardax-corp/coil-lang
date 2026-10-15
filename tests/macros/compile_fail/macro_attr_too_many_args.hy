// Expected: E0119 — `add_after` takes one argument after the function.
use derive_macros::{add_after};

#[add_after(1, 2)]
fn triple(int x) -> int {
    return x * 3;
}

fn main() {}
