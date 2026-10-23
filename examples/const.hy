// `const` locals.
//
// @output 42hi

use io::sync::print;
use string::format;

fn main() {
    const answer = 42;
    let _ = print(format("%i", answer));
    const greeting = "hi";
    let _ = print(format("%s", greeting));
}
