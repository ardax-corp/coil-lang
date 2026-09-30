use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn sadge(string n) {
    write_all(stdout(), to_bytes(format("%s", n)));
}

fn main() {
    sadge("Hello");
}
