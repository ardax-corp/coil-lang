// `byte` / `Vec<byte>` basics used by the IO layer.
//
// @output 25532

use io::sync::print;
use string::format;

fn main() {
    let b: byte = 255;
    let arr = Vec::from([1 as byte, 2 as byte, 3 as byte]);
    let _ = print(format("%i", b));
    let _ = print(format("%i", arr.len()));
    let _ = print(format("%i", arr[1]));
}
