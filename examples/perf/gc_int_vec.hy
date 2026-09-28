// GC: a large live Vec<int> through heavy tuple churn. Marking scans the
// vector once, finds no references, and skips it until it is written again
// (ObjArray::may_hold_refs).
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn fill(int n) -> Vec<int> {
    let v: Vec<int> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        v.push(i);
        i = i + 1;
    }
    return v;
}

fn main() {
    let big = fill(2000000);
    let total = 0;
    let round = 0;
    while round < 2000000 {
        let t = (round, round + 1);
        total = total + t[1] - t[0];
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i %i", total, len(big))));
}
