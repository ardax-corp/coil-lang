// Fixed-N vs rest-K (N < K) must dispatch by argc: exact fixed wins at N;
// rest handles K and above. Typechecker unit tests alone miss wrong CALL targets.
// @output 1035
use io::{stdout, write};
use string::{format, to_bytes};

fn f(int x) -> int {
    return x * 10;
}

fn f(int x, int y, int... xs) -> int {
    return x + y + xs.len();
}

fn main() {
    let _ = write(stdout(), to_bytes(format("%i", f(1))));
    let _ = write(stdout(), to_bytes(format("%i", f(1, 2))));
    let _ = write(stdout(), to_bytes(format("%i", f(1, 2, 3, 4))));
}
