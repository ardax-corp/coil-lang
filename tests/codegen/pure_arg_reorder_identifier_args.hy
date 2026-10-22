// @output 7,12,12
use io::{stdout, write};
use string::{format, to_bytes};

fn effect() -> int {
    let _ = write(stdout(), to_bytes(format("%i,", 7)));
    return 2;
}

fn sink(int a, int b) -> int {
    let _ = write(stdout(), to_bytes(format("%i,", a + b)));
    return a + b;
}

fn main() {
    let cached = 10;
    let _ = write(stdout(), to_bytes(format("%i", sink(effect(), cached))));
}
