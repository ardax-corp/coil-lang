// Spreading a `let`-bound tuple or array fills the parameters in element
// order, the same as spreading a literal.
// Output: 6
use io::{stdout, write};
use string::{format, to_bytes};

fn add3(int a, int b, int c) -> int {
    return a + b + c;
}

fn main() {
    let pack = (1, 2, 3);
    write(stdout(), to_bytes(format("%i", add3(...pack))));
}
