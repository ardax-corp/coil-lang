// examples/perf/numeric.hy run as a test: the checksum pins the shape below.
// Output: 1999000

// CPU: iterative arithmetic + control flow (while, compound assign).
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let acc = 0;
    let i = 0;
    while i < 2000 {
        acc = acc + i;
        i = i + 1;
    }
    let _ = write_all(stdout(), to_bytes(format("%i", acc)));
}
