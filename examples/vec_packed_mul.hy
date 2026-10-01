// Packed aggregate SIMD path — static length ≥ 8 uses HostInvoke.
//
// Output: 246810121416,3691215182124

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let a = [1, 2, 3, 4, 5, 6, 7, 8];
    let b = [2, 2, 2, 2, 2, 2, 2, 2];
    let p = a * b;
    write_all(
        stdout(),
        to_bytes(format("%i%i%i%i%i%i%i%i,", p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7])),
    );
    let s = a * 3;
    write_all(
        stdout(),
        to_bytes(format("%i%i%i%i%i%i%i%i", s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7])),
    );
}
