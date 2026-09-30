// CPU: `while i < len(v)` scan + in-place fill — the P2 counted-loop shape.
// `len(v)` is invariant across element writes, so it hoists to the preheader.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn fill(Vec<int> v) -> int {
    let i = 0;
    while i < len(v) {
        v[i] = i;
        i = i + 1;
    }
    return len(v);
}

fn scan(Vec<int> v) -> int {
    let acc = 0;
    let i = 0;
    while i < len(v) {
        acc = acc + v[i];
        i = i + 1;
    }
    return acc;
}

fn main() {
    let v: Vec<int> = Vec::with_capacity(1 << 12);
    let i = 0;
    while i < (1 << 12) {
        v.push(0);
        i = i + 1;
    }
    let total = 0;
    let round = 0;
    while round < 64 {
        fill(v);
        total = total + scan(v);
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
