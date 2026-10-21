// Under-applying a function with named arguments builds a partial whose
// remaining holes fill in declaration order when it is called.
// Output: 3
use io::{stdout, write};
use string::{format, to_bytes};

fn add(int a, int b) -> int {
    return a + b;
}

fn main() {
    let g = add(a: 1);
    let _ = write(stdout(), to_bytes(format("%i", g(2))));
}
