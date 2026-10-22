// @expected E0119 — `add_after` has no parameter `times`.
use derive_macros::add_after;

#[add_after(by = 1, times = 2)]
fn triple(int x) -> int {
    return x * 3;
}

fn main() {}
