// S2l leftover: in-loop MakeArray that escapes (SROA cannot delete).
// N=2000000; checksum n^2 = 4000000000000.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn take([int] xs) -> int {
    return xs[0] + xs[1];
}

fn pack(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        s = s + take([i, i + 1]);
        i = i + 1;
    }
    return s;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", pack(2000000))));
}
