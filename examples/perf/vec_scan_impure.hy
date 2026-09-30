// CPU: `while i < len(v) { f(v[i]) }` where `f` is impure but cannot resize
// an array — it only writes a field. `len(v)` stays invariant across the CALL,
// so it hoists and `v[i]` unchecks, as for a pure helper (vec_scan_pure.hy).
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

class Tally {
    pub hits: int,
}

fn tally() -> Tally {
    return new Tally(0);
}

fn absorb(Tally t, int x) -> int {
    let k = 0;
    let s = x;
    while k < 8 {
        s = s + k;
        k = k + 1;
    }
    t.hits = t.hits + 1;
    return s;
}

fn fill(Vec<int> v) -> int {
    let i = 0;
    while i < len(v) {
        v[i] = i;
        i = i + 1;
    }
    return len(v);
}

fn scan(Tally t, Vec<int> v) -> int {
    let acc = 0;
    let i = 0;
    while i < len(v) {
        acc = acc + absorb(t, v[i]);
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
    let t = tally();
    let total = 0;
    let round = 0;
    while round < 64 {
        fill(v);
        total = total + scan(t, v);
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i %i", total, t.hits)));
}
