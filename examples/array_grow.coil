// `Vec::from` an array literal, then grow it with `push`.
//
// @output 414

use io::sync::print;
use string::format;

fn main() {
    let a = Vec::from([1, 2]);
    a.push(3);
    a.push(4);
    let _ = print(format("%i", a.len()));
    let _ = print(format("%i", a[0]));
    let _ = print(format("%i", a[3]));
}
