// Literal stdout smoke test.
//
// Exercises writing a string literal through io::stdout and string::to_bytes.
//
// Output: hello

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    write_all(stdout(), to_bytes("hello"));
}
