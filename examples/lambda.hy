// An anonymous `fn` that captures a local with `use (…)`.
//
// @output 42

use io::sync::print;
use string::format;

fn main() {
    let y = 10;
    let f = fn (int x) use (y) => x + y;
    let _ = print(format("%i", f(32)));
}
