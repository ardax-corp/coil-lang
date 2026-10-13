// `break` disables unroll — first iteration still runs, rest skipped.
// Output: 0
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let s = 0;
    let i = 0;
    while i < 5 {
        s = s + i;
        break;
    }
    write(stdout(), to_bytes(format("%i", s)));
}
