// Top-level named `fn`s are callable from a lambda without `use` (empty captures).
// Output: 42
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn rec(int n) -> int {
    return n + 1;
}

fn main() {
    let f = fn (int x) => rec(x);
    let _ = write_all(stdout(), to_bytes(format("%i", f(41))));
}
