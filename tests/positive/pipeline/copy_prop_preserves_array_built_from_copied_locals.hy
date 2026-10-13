// Copied locals feeding MakeArray must not be rewritten into shape-sensitive
// stack producers that break array construction.
// Output: 30
use io::{stdout, write};
use string::{format, to_bytes};

fn build() -> int {
    let a = 10;
    let b = a;
    let xs = [b, 20];
    return xs[0] + xs[1];
}

fn main() {
    write(stdout(), to_bytes(format("%i", build())));
}
