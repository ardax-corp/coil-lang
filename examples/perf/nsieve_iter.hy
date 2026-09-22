// Iterative sibling of nsieve.hy. Same prime count 1900 for n = 1 << 14.
// The ones-fill is its own stride-1 store. The sieve itself still steps
// by p and branches on flags[p], so that loop is not vectorized.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn fill_ones(Vec<int> flags) -> int {
    let n = len(flags);
    let i = 0;
    while i < n {
        flags[i] = 1;
        i = i + 1;
    }
    return n;
}

fn nsieve_iter(int n) -> int {
    let flags: Vec<int> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        flags.push(0);
        i = i + 1;
    }
    let _ = fill_ones(flags);
    let count = 0;
    let p = 2;
    while p < n {
        if flags[p] == 1 {
            count = count + 1;
            let k = p + p;
            while k < n {
                flags[k] = 0;
                k = k + p;
            }
        }
        p = p + 1;
    }
    return count;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", nsieve_iter(1 << 14))));
}
