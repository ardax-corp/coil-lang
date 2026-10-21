// Expected: E0102 — `invoke`'s function id is the `int` a `declare` returned;
// `doubler` is a Coil `fn`, not a foreign one (#1666).
use ffi::invoke;

fn doubler(int x) -> int {
    return x * 2;
}

fn main() {
    let _ = invoke(7, doubler, ());
}
