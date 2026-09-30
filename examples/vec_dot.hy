// Named helpers — `dot` and `cross` on homogeneous vectors.
// Expected output: 32,001

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let d = dot((1, 2, 3), (4, 5, 6));
    write_all(stdout(), to_bytes(format("%i,", d)));
    let c = cross((1, 0, 0), (0, 1, 0));
    write_all(stdout(), to_bytes(format("%i%i%i", c[0], c[1], c[2])));
}
