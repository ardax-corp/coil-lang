// `%v` formats tuples and records.
//
// @output (1, 2){ a: 3, b: 4 }

use io::sync::print;
use string::format;

fn main() {
    let _ = print(format("%v", (1, 2)));
    let _ = print(format("%v", { a: 3, b: 4 }));
}
