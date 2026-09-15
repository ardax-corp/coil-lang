// Hit bench for COI-382 S5: stride DenseStoreIndex + IV bump.
// Flagship nsieve k-loop is the same shape; n=1<<14 is ~2.5ms wall (wash).
// Fill once, then many stride passes so the store+IV latch dominates.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn hot(int n, int p, int reps) -> int {
    let flags: Vec<int> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        flags.push(1);
        i = i + 1;
    }
    let r = 0;
    while r < reps {
        let k = p;
        while k < n {
            flags[k] = 0;
            k = k + p;
        }
        r = r + 1;
    }
    return flags[0] + flags[p] + flags[n - 1];
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", hot(4096, 3, 20000))));
}
