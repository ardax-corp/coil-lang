// Formatted stdout smoke test.
//
// Exercises string::format plus io::stdout/write_all.
//
// @output 42

use io::stdout;
use io::sync::print;
use io::sync::write_all;
use string::format;

fn main() {
    let _ = print(format("%i", 42));
}
