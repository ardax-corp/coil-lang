// CPU: integer loops + array mutation (cross-lang fair bench).
// Sieve of Eratosthenes; n = 1<<14; prints prime count.
use io::sync::print;
use string::format;

fn nsieve(int n) -> int {
    let flags: Vec<int> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        flags.push(1);
        i = i + 1;
    }
    let count = 0;
    let p = 2;
    while p < n {
        if flags[p] == 1 {
            count = count + 1;
            let k = p + p;
            while k < n {
                flags[k] = 0;
                k = k + p;
            }
        }
        p = p + 1;
    }
    return count;
}

fn main() {
    let _ = print(format("%i", nsieve(1 << 14)));
}
