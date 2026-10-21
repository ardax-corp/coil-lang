// Output: 3
use io::{stdout, write};
use string::{format, to_bytes};

fn add<T: Num>(T a, T b) -> T {
    return a + b;
}

fn main() {
    let _ = write(stdout(), to_bytes(format("%i", add(1, 2))));
}
