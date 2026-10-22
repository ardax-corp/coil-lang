// Fixed `[T; N]` stack locals + heap `Vec<T>` method sugar.
//
// @output 20,3,99,2

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let fixed = [10, 20, 30];
    let _ = write_all(stdout(), to_bytes(format("%i,", fixed[1])));

    let v: Vec<int> = Vec::new();
    v.push(fixed[0]);
    v.push(fixed[1]);
    v.push(fixed[2]);
    let _ = write_all(stdout(), to_bytes(format("%i,", v.len())));

    v[1] = 99;
    let _ = write_all(stdout(), to_bytes(format("%i,", v[1])));

    let _ = v.pop();
    let _ = write_all(stdout(), to_bytes(format("%i", v.len())));
}
