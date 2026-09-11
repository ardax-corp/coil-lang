// F2 n-ary associative combine (COI-368): trib(n-1)+trib(n-2)+trib(n-3).
// F1 only admitted binary `⊕`. Sequential A4: COIL_AUTO_PAR=0.
// Checksum: trib(26) = 3311233 with trib(n<=2)=1.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn trib(int n) -> int {
    if n <= 2 {
        return 1;
    }
    return trib(n - 1) + trib(n - 2) + trib(n - 3);
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", trib(26))));
}
