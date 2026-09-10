// C2 / Q6: Range parameter as counted cur/end (no heap GetField).
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn range_sum(Range<int> r) -> int {
    let iter = r;
    let acc = 0;
    for x in iter {
        acc = acc + x;
    }
    return acc;
}

fn main() {
    let total = 0;
    let round = 0;
    while round < 96 {
        total = total + range_sum(0..(1 << 14));
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
