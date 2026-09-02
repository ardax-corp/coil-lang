// Canary (gate 2): for-in pin over a large int array, distinct from counted
// vec_scan (`while i < len(v)`). A later for-in pin cut should drop VM-only time.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn main() {
    let n = 1 << 14;
    let v: Vec<int> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        v.push(i);
        i = i + 1;
    }
    let total = 0;
    let round = 0;
    while round < 96 {
        let acc = 0;
        for x in v {
            acc = acc + x;
        }
        total = total + acc;
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
