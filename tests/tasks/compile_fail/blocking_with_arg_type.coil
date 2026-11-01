// @expected E0102 — `task::blocking_with(f, arg)` passes `arg` to `f`, so
// its type must match `f`'s parameter.
use task::blocking_with;

fn twice(int n) -> int {
    return n * 2;
}

fn main() {
    let _ = blocking_with(twice, "two");
}
