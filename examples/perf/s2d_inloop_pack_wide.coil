// In-loop MakeArray of arity 8 (heavier Alloc, same trip count).
use io::sync::print;
use string::format;

fn pack(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        let xs = [i, i + 1, i + 2, i + 3, i + 4, i + 5, i + 6, i + 7];
        s = s + xs[i % 8];
        i = i + 1;
    }
    return s;
}

fn main() {
    let _ = print(format("%i", pack(500000)));
}
