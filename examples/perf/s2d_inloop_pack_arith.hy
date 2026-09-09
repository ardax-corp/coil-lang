// In-loop MakeArray mixed with extra integer arithmetic.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn pack(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        let xs = [i, i + 1, i + 2];
        s = s + xs[i % 3] * 3 + 1;
        i = i + 1;
    }
    return s;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", pack(2000000))));
}
