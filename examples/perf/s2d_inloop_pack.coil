// S2d A/B shape; S2f SROA deletes MakeArray (S2k dense select).
// N=2000000; expected checksum 2000000999999.
use io::sync::print;
use string::format;

fn pack(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        let xs = [i, i + 1, i + 2];
        s = s + xs[i % 3];
        i = i + 1;
    }
    return s;
}

fn main() {
    let _ = print(format("%i", pack(2000000)));
}
