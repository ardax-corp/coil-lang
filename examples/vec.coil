// Fixed `[T; N]` stack locals + heap `Vec<T>` method sugar.
//
// @output 20,3,99,2

use io::sync::print;
use string::format;

fn main() {
    let fixed = [10, 20, 30];
    let _ = print(format("%i,", fixed[1]));

    let v: Vec<int> = Vec::new();
    v.push(fixed[0]);
    v.push(fixed[1]);
    v.push(fixed[2]);
    let _ = print(format("%i,", v.len()));

    v[1] = 99;
    let _ = print(format("%i,", v[1]));

    let _ = v.pop();
    let _ = print(format("%i", v.len()));
}
