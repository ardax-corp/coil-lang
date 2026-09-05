// Hit bench for local EarlyCSE: force Index recomputes InstCombine +
// cfg_gvn/ssa_gvn do not already fold (varying index, stored first hit).
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

fn hot(Vec<int> v, int n) -> int {
    let s = 0;
    let i = 0;
    while i < n {
        let k = i & 63;
        let x = v[k];
        s = s + x;
        s = s + v[k];
        s = s + v[k];
        s = s + v[k];
        i = i + 1;
    }
    return s;
}

fn main() {
    let v = fill(64);
    write_all(stdout(), to_bytes(format("%i", hot(v, 4000000))));
}
