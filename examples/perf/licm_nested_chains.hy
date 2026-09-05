// CPU: two independent invariant int chains in nested loops.
// LICM iterate must hoist both; a single-chain-then-return pass leaves one.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn nested(int n) -> int {
    let s = 0;
    let y = 0;
    while y < n {
        let x = 0;
        while x < n {
            s = s + (n * 3 + 1);
            s = s + (n * 5 + 2);
            x = x + 1;
        }
        y = y + 1;
    }
    return s;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", nested(2000))));
}
