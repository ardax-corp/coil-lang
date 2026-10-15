// Same induction check with `i <= 2` (3 trips: 0+1+2).
// Output: 3
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let s = 0;
    let i = 0;
    while i <= 2 {
        s = s + i;
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", s)));
}
