// Hit bench for the widened stride-1 vectorizer: sum of the index (no load),
// a loop that does not start at 0, and a[i + 1].
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn sum_i(int n) -> int {
    let s = 0;
    let i = 0;
    while i < n {
        s = s + i;
        i = i + 1;
    }
    return s;
}

fn sum_from(Vec<int> v, int start) -> int {
    let s = 0;
    let i = start;
    while i < len(v) {
        s = s + v[i];
        i = i + 1;
    }
    return s;
}

fn sum_next(Vec<int> v) -> int {
    let s = 0;
    let i = 0;
    while i < len(v) - 1 {
        s = s + v[i + 1];
        i = i + 1;
    }
    return s;
}

fn main() {
    let v: Vec<int> = Vec::with_capacity(1024);
    let i = 0;
    while i < 1024 {
        v.push(i);
        i = i + 1;
    }
    let total = sum_i(1024) + sum_from(v, 8) + sum_next(v);
    write_all(stdout(), to_bytes(format("%i", total)));
}
