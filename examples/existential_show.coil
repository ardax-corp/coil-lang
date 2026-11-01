// Bare-class existential: `Show` as a value type.
//
// @output 42

use io::sync::print;
use string::format;

fn print_any(Show x) {
    let _ = print(format("%s", show(x)));
}

fn main() {
    print_any(42);
}
