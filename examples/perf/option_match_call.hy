// B3 hit bench: two-slot CALL + match + arith in a counted loop.
// Checksum: period-10 lookup(i%10, 7) → Some(i*2) for i%10 < 7 else 0.
// Sum per 10 = 0+2+4+6+8+10+12+0+0+0 = 42; 2e6 periods → 84000000.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn lookup(int i, int n) -> Option<int> {
    if i < 0 || i >= n {
        return Option::None;
    }
    return Option::Some(i * 2);
}

fn hot(int n, int iters) -> int {
    let acc = 0;
    let i = 0;
    while i < iters {
        acc = acc + match lookup(i % 10, n) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        i = i + 1;
    }
    return acc;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", hot(7, 20000000))));
}
