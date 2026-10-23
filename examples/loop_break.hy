// `break` and `continue` in a `while` loop.
//
// @output 18

use io::sync::print;
use string::format;

fn main() {
    let sum = 0;
    let i = 0;
    while i < 10 {
        if i == 3 {
            i = i + 1;
            continue;
        }
        if i == 7 {
            break;
        }
        sum = sum + i;
        i = i + 1;
    }
    let _ = print(format("%i", sum));
}
