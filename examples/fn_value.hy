use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn add(int a, int b) -> int {
    return a + b;
}

fn main() {
    let f = add;
    write_all(stdout(), to_bytes(format("%i", f(20, 22))));
    let g = add(1);
    write_all(stdout(), to_bytes(format("%i", g(2))));
}
