// F2 n-ary associative helper combine (COI-368): fib(n)+fib(n-1)+fib(n-2).
// F1 only matched the inner binary `fib(n)+fib(n-1)` as the site.
// Sequential A4: COIL_AUTO_PAR=0.
// Checksum: fib(32)+fib(31)+fib(30) = 4356618 with fib(n<=2)=1.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn fib(int n) -> int {
    if n <= 2 {
        return 1;
    }
    return fib(n - 1) + fib(n - 2);
}

fn triple_fib(int n) -> int {
    if n <= 0 {
        return 0;
    }
    return fib(n) + fib(n - 1) + fib(n - 2);
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", triple_fib(32))));
}
