// `byte` / `Vec<byte>` basics used by the IO layer.
//
// Output: 25532

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let b: byte = 255;
    let arr = Vec::from([1 as byte, 2 as byte, 3 as byte]);
    write_all(stdout(), to_bytes(format("%i", b)));
    write_all(stdout(), to_bytes(format("%i", len(arr))));
    write_all(stdout(), to_bytes(format("%i", arr[1])));
}
