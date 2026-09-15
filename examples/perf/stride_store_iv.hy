// Hit bench for COI-382 S5: stride DenseStoreIndex + IV bump.
// Flagship nsieve k-loop is the same shape; n=1<<14 is ~2.5ms wall (wash).
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn hot(int n, int p) -> int {
    let flags: Vec<int> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        flags.push(1);
        i = i + 1;
    }
    let k = p;
    while k < n {
        flags[k] = 0;
        k = k + p;
    }
    return flags[0] + flags[p] + flags[n - 1];
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", hot(1000000, 2))));
}
