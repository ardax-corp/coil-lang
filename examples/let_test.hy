// examples/let_test.hy — `let` bindings and re-assignment.
//
// @output 51020

use io::sync::print;
use string::format;

fn main() {
    let x = 5;
    let _ = print(format("%i", x));
    let y = 10;
    let _ = print(format("%i", y));
    x = 20;
    let _ = print(format("%i", x));
}
