// COI-372 hit bench: proven DenseIndex/DenseStoreIndex with stable array
// identity. Stride 3 keeps SIMD VLoad/VStore off so the slab probe is in the
// hot path. Flagship nsieve `.hyc` is startup-bound (~2ms); this is more trips.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn paint(Vec<int> a, int step) -> int {
    let k = 0;
    while k < len(a) {
        a[k] = a[k] + 1;
        k = k + step;
    }
    return a[0];
}

fn main() {
    let n = 1 << 18;
    let a: Vec<int> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        a.push(0);
        i = i + 1;
    }
    let acc = 0;
    let round = 0;
    while round < 48 {
        acc = acc + paint(a, 3);
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", acc)));
}
