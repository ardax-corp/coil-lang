// Named helper — `matmul` on nested fixed-length matrices (row-major).
// Expected output: 19,22,43,50

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let a = [[1, 2], [3, 4]];
    let b = [[5, 6], [7, 8]];
    let c = matmul(a, b);
    write_all(stdout(), to_bytes(format("%i,%i,%i,%i", c[0][0], c[0][1], c[1][0], c[1][1])));
}
