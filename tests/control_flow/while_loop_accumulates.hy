// Output: 4950
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let acc = 0;
    let i = 0;
    while i < 100 {
        acc = acc + i;
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", acc)));
}
