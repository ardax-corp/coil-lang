// C1 hit bench: self two-slot Option CALL + match + arith.
// walk(k) = Some(k). hot(8, iters) sums (i % 8) over iters.
// 1e6 periods of 0+1+…+7 = 28 → 28000000.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

#[max_depth(16)]
fn walk(int n) -> Option<int> {
    if n <= 0 {
        return Option::Some(0);
    }
    let r = walk(n - 1);
    return match r {
        Option::Some(x) => Option::Some(x + 1),
        Option::None => Option::None,
    };
}

fn hot(int n, int iters) -> int {
    let acc = 0;
    let i = 0;
    while i < iters {
        acc = acc + match walk(i % n) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        i = i + 1;
    }
    return acc;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", hot(8, 8000000))));
}
