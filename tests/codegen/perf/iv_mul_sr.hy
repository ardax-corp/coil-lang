// examples/perf/iv_mul_sr.hy run as a test: the checksum pins the shape below.
// Output: 31499989500000

// CPU: force integer IV strength reduction (`i * c` → add recurrence).
// Odd invariant factor so codegen does not rewrite the mul to SHL.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn iv_mul(int n, int c) -> int {
    let s = 0;
    let i = 0;
    while i < n {
        s = s + i * c;
        i = i + 1;
    }
    return s;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", iv_mul(3000000, 7))));
}
