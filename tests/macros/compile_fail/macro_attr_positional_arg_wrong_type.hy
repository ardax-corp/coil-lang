// @expected E0119 — a positional attribute argument is checked against its
// parameter's type too: `by` takes an `int`, not a `bool` (#1201).
use derive_macros::add_after;

#[add_after(true)]
fn triple(int x) -> int {
    return x * 3;
}

fn main() {}
