// Compound assignment, `++` / `--`, and `**`.
//
// @output 801125428falsetrue3

use io::sync::print;
use string::format;

fn main() {
    let x = 5;
    x += 3;
    let _ = print(format("%i", x));

    let y = 0;
    let _ = print(format("%i", y++));
    let _ = print(format("%i", y));

    let z = 0;
    let _ = print(format("%i", ++z));

    let arr = [10, 20, 30];
    arr[1] += 5;
    let _ = print(format("%i", arr[1]));

    let d = { val: 1 };
    d.val += 41;
    let _ = print(format("%i", d.val));

    let _ = print(format("%i", 2 ** 3));
    let _ = print(format("%z", true && false));
    let _ = print(format("%z", 5 != 4));
    let _ = print(format("%i", 7 & 3));
}
