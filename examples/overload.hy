use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn add(int x) -> int {
    return x;
}

fn add(int x, int y) -> int {
    return x + y;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", add(1))));
    write_all(stdout(), to_bytes(format("%i", add(2, 3))));
}
