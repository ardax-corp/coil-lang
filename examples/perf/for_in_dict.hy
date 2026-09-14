// C2b / Q6 rung 3: dict for-in as DictEntries then counted array latch.
// Helper sums entry values (`p[1]`). `main` stays format. Coro for-in
// is still refuse (rung 4).
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn dict_sum() -> int {
    let acc = 0;
    let d = { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6, g: 7, h: 8 };
    for p in d {
        acc = acc + p[1];
    }
    return acc;
}

fn main() {
    let total = 0;
    let round = 0;
    while round < 96 {
        total = total + dict_sum();
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
