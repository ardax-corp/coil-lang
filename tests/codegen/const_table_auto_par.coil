// #1561: a loop auto-par splits across workers may read a module `const`
// set at startup and a `const` table: a shared-heap worker reads the
// statics of the VM that spawned it. A function-local table its first
// call builds in a worker is that worker's own.
// @output 239999400000 360001200000 32838255328
use io::{stdout, write};
use string::{format, to_bytes};

fn three() -> int {
    return 3;
}

const THRICE: int = three();

const WEIGHTS = readonly [1, 2, 3, 4, 5, 6, 7, 8];

fn tripled(int i) -> int {
    return i * THRICE;
}

fn weighted(int i) -> int {
    return i * WEIGHTS[i % 8];
}

fn sbox16(int x) -> int {
    const SBOX = readonly [12, 5, 6, 11, 9, 0, 10, 13, 3, 14, 15, 8, 4, 7, 1, 2];
    let y = 0;
    for k in 0..4 {
        y = y | (SBOX[(x >> (4 * k)) & 15] << (4 * k));
    }
    return y;
}

fn local_sum() -> int {
    let c = 0;
    for i in 0..1000000 {
        c = c + sbox16(i & 65535);
    }
    return c;
}

fn main() {
    let a = 0;
    for i in 0..400000 {
        a = a + tripled(i);
    }
    let b = 0;
    for i in 0..400000 {
        b = b + weighted(i);
    }
    let _ = write(stdout(), to_bytes(format("%i %i %i", a, b, local_sum())));
}
