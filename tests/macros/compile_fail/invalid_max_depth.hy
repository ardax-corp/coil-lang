// Expected: E0119 — #[max_depth(N)] requires a positive integer.
#[max_depth(0)]
fn fib(int n) -> int {
    if n <= 2 {
        return 1;
    }
    return fib(n - 1) + fib(n - 2);
}

fn main() {
    let k = 10;
    let _ = fib(k);
}
