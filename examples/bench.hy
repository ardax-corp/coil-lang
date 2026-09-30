// examples/bench.hy — minimal smoke-test (not a real benchmark)
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let a = 5;
    let b = 7;
    let c = a + b;
    write_all(stdout(), to_bytes(format("%i\n", c)));
}
