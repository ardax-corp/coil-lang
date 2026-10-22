// Formatted stdout smoke test.
//
// Exercises string::format plus io::stdout/write_all.
//
// @output 42

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let _ = write_all(stdout(), to_bytes(format("%i", 42)));
}
