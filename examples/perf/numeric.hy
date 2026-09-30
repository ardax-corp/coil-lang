// CPU: iterative arithmetic + control flow (while, compound assign).
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let acc = 0;
    let i = 0;
    while i < 2000 {
        acc = acc + i;
        i = i + 1;
    }
    write_all(stdout(), to_bytes(format("%i", acc)));
}
