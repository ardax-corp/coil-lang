// Output: 4,2
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let i = 0;
    let n = 0;
    while i < 4 {
        if i < 2 {
            n = n + 1;
        }
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i,%i", i, n)));
}
