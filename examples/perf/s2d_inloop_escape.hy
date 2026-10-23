// S2l leftover: in-loop MakeArray that escapes (SROA cannot delete).
// N=2000000; checksum n^2 = 4000000000000.
use io::sync::print;
use string::format;

fn take(Vec<int> xs) -> int {
    return xs[0] + xs[1];
}

fn pack(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        s = s + take([i, i + 1]);
        i = i + 1;
    }
    return s;
}

fn main() {
    let _ = print(format("%i", pack(2000000)));
}
