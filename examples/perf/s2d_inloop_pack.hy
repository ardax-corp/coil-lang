// S2d A/B: in-loop MakeArray + computed index (the ~18% shape).
// N=2000000; expected checksum 2000000999999.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn pack(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        let xs = [i, i + 1, i + 2];
        s = s + xs[i % 3];
        i = i + 1;
    }
    return s;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", pack(2000000))));
}
