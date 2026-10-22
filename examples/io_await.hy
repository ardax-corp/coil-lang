// Exercise io::drive and await_* registration (no hung wait).
//
// @output 0

use io::stdout;
use io::drive;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let n = drive();
    let _ = write_all(stdout(), to_bytes(format("%i", n)));
}
