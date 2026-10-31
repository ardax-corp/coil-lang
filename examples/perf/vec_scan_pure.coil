// CPU: `while i < v.len() { f(v[i]) }` with a non-inlined pure helper — COI-99.
// `v.len()` is invariant across the CALL, so it hoists; `v[i]` may uncheck.
use io::sync::print;
use string::format;

fn absorb(int x) -> int {
    let k = 0;
    let t = x;
    while k < 8 {
        t = t + k;
        k = k + 1;
    }
    return t;
}

fn fill(Vec<int> v) -> int {
    let i = 0;
    while i < v.len() {
        v[i] = i;
        i = i + 1;
    }
    return v.len();
}

fn scan(Vec<int> v) -> int {
    let acc = 0;
    let i = 0;
    while i < v.len() {
        acc = acc + absorb(v[i]);
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
    let _ = print(format("%i", total));
}
