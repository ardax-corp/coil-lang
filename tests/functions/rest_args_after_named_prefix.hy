// Rest packing with a named fixed prefix + trailing positionals (P4 + P2).
// Output: 137
use io::{stdout, write};
use string::{format, to_bytes};

fn f(int a, int... xs) -> int {
    return a + xs.len();
}

fn main() {
    let _ = write(stdout(), to_bytes(format("%i", f(a: 10, 1, 2, 3))));
    let _ = write(stdout(), to_bytes(format("%i", f(a: 7))));
}
