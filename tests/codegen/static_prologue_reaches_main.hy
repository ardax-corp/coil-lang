// A static initializer's setup reaches `main`, not an earlier recursive
// function that reused `main`'s label.
// Output: main ran
use io::stdout;
use io::sync::write_all;
use string::to_bytes;

fn fib(int n) -> int {
    if n < 2 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

static let k: int = 4;

fn main() {
    if fib(k) == 3 {
        write_all(stdout(), to_bytes("main ran"))?;
    }
}
