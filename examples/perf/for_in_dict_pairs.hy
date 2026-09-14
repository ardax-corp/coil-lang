// COI-371: dict for-in via `for (k, v)` — same DictEntries counted latch as `p[1]`.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn dict_sum() -> int {
    let acc = 0;
    let d = { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6, g: 7, h: 8 };
    for (k, v) in d {
        acc = acc + v;
    }
    return acc;
}

fn main() {
    let total = 0;
    let round = 0;
    while round < 96 {
        total = total + dict_sum();
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
