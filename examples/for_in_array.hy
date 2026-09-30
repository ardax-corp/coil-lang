// Expected output: 123
//
// `for x in` over an array — IntoIterator synthesises Item = element type.

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    for x in [1, 2, 3] {
        write_all(stdout(), to_bytes(format("%i", x)));
    }
}
