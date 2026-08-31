// Expected output: 7

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
trait Tiny<T> {
    fn base(T x) -> int;

    fn next(T x) -> int {
        return base(x) + 1;
    }
}

impl Tiny<int> {
    pub fn base(int x) -> int {
        return x;
    }
}

fn get<T: Tiny>(T x) -> int {
    return next(x);
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", get(41))));
}
