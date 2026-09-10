// C2 / Q6: returned Range as two-slot [start, end], then counted for-in.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn make_range(int n) -> Range<int> {
    return 0..n;
}

fn range_sum(int n) -> int {
    let r = make_range(n);
    let acc = 0;
    for x in r {
        acc = acc + x;
    }
    return acc;
}

fn main() {
    let total = 0;
    let round = 0;
    while round < 96 {
        total = total + range_sum(1 << 14);
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
