// Two-slot (int, int) CALL/RETURN: helper leaves [a, b] without boxing
// ObjTuple. ITERS=20000000; period-10 checksum 100 * 2e6 = 200000000.
// Release VM-only ~1-3s.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn pair(int i) -> (int, int) {
    let k = i % 10;
    return (k, k + 1);
}

fn main() {
    let iters = 20000000;
    let acc = 0;
    let i = 0;
    while i < iters {
        let (a, b) = pair(i);
        acc = acc + a + b;
        i = i + 1;
    }
    write_all(stdout(), to_bytes(format("%i", acc)));
}
