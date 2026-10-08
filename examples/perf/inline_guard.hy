// Guard-clause callees in a hot loop: early `return`s fold into one `if`
// value, so typed inlining can splice `clamp` and `step` into the loop.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn clamp(int x, int lo, int hi) -> int {
    if x < lo {
        return lo;
    }
    if x > hi {
        return hi;
    }
    return x;
}

fn step(int x) -> int {
    if x % 3 == 0 {
        return x / 3;
    }
    return x + 1;
}

fn run() -> int {
    let acc = 0;
    let i = 0;
    while i < 3000000 {
        acc = acc + clamp(i % 1000, 100, 900) + step(i % 7);
        i = i + 1;
    }
    return acc;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", run())));
}
