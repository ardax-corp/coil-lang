// Output: 6
use io::{stdout, write};
use string::{format, to_bytes};

fn add3(int a, int b, int c) -> int {
    return a + b + c;
}

fn main() {
    let p = add3(1);
    let _ = write(stdout(), to_bytes(format("%i", p(...(2, 3)))));
}
