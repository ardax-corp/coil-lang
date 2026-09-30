// Expected output: (1, 2){ a: 3, b: 4 }
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    write_all(stdout(), to_bytes(format("%v", (1, 2))));
    write_all(stdout(), to_bytes(format("%v", { a: 3, b: 4 })));
}
