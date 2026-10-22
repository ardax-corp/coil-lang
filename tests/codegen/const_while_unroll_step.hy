// Constant-trip `for` unroll must advance `i` via `step` between bodies.
// If unroll engaged but skipped `step`, `s = s + i` would stay `0`.
// @output 6
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let s = 0;
    let i = 0;
    while i < 4 {
        s = s + i;
        i = i + 1;
    }
    let _ = write(stdout(), to_bytes(format("%i", s)));
}
