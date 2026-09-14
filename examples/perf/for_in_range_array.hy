// C2b / Q6 rung 1: array-held Range as counted cur/end (no GetField).
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn range_sum([Range<int>] rs) -> int {
    let acc = 0;
    for x in rs[0] {
        acc = acc + x;
    }
    return acc;
}

fn main() {
    let total = 0;
    let round = 0;
    while round < 96 {
        total = total + range_sum([0..(1 << 14)]);
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
