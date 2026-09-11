// Counted for-range IPA hit bench (COI-367 F0). Const trip count so loop
// grain applies (dynamic `0..n` stays sequential). Sequential A4: COIL_AUTO_PAR=0.
// Checksum: sum_{x=0}^{199999} x*x = 2666646666700000.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn sq(int i) -> int {
    return i * i;
}

fn main() {
    let acc = 0;
    for x in 0..200000 {
        acc = acc + sq(x);
    }
    write_all(stdout(), to_bytes(format("%i", acc)));
}
