// @output 8
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let s = 0;
    for x in [1, 2, 3, 4] {
        if x == 2 {
            continue;
        }
        s = s + x;
    }
    let _ = write(stdout(), to_bytes(format("%i", s)));
}
