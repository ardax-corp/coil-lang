// examples/let_test.hy — `let` bindings and re-assignment.
//
// Output: 51020

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let x = 5;
    let _ = write_all(stdout(), to_bytes(format("%i", x)));
    let y = 10;
    let _ = write_all(stdout(), to_bytes(format("%i", y)));
    x = 20;
    let _ = write_all(stdout(), to_bytes(format("%i", x)));
}
