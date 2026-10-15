// Output: 12
use io::{stdout, write};
use string::{format, to_bytes};

fn sum_alias() -> int {
    let seed = 3;
    let alias = seed;
    let i = 0;
    let total = 0;
    while i < 4 {
        total = total + alias;
        i = i + 1;
    }
    return total;
}

fn main() {
    write(stdout(), to_bytes(format("%i", sum_alias())));
}
