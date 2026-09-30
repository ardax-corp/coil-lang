// Numeric tower — static array zip / scalar broadcast.
// Literals infer `[int; N]`, so zip is allowed. Dynamic `[T] ⊕ [T]` is a
// hard type error (see diagnostics tests).
// Expected output: 46,45,18

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let a = [1, 2] + [3, 4];
    write_all(stdout(), to_bytes(format("%i%i,", a[0], a[1])));
    let b = [1, 2] + 3;
    write_all(stdout(), to_bytes(format("%i%i,", b[0], b[1])));
    let c = [1, 2] ** 3;
    write_all(stdout(), to_bytes(format("%i%i", c[0], c[1])));
}
