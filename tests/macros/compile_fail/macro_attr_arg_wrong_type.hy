// Expected: E0119 — `add_after`'s `by` takes an `int`, so a string argument
// is an error at the attribute instead of a garbage integer (#1201).
use derive_macros::add_after;

#[add_after(by = "ten")]
fn triple(int x) -> int {
    return x * 3;
}

fn main() {}
