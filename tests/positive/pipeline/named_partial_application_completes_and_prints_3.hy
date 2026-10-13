// Named under-apply must build a partial whose holes fill in declaration
// order at CallIndirect time (not print the named value as the only arg).
// Output: 3
use io::{stdout, write};
use string::{format, to_bytes};

fn add(int a, int b) -> int {
    return a + b;
}

fn main() {
    let g = add(a: 1);
    write(stdout(), to_bytes(format("%i", g(2))));
}
