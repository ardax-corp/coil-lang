// Q6: for-in pin over a large int Vec. The `sum` helper is the counted
// island (same shape as `while i < len`). `main` keeps fill + format.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn sum(Vec<int> v) -> int {
    let acc = 0;
    for x in v {
        acc = acc + x;
    }
    return acc;
}

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
        total = total + sum(v);
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
