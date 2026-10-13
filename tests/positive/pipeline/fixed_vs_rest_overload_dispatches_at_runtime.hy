// Fixed-N vs rest-K (N < K) must dispatch by argc: exact fixed wins at N;
// rest handles K and above. Typechecker unit tests alone miss wrong CALL targets.
// Output: 1035
use io::{stdout, write};
use string::{format, to_bytes};

fn f(int x) -> int {
    return x * 10;
}

fn f(int x, int y, int... xs) -> int {
    return x + y + len(xs);
}

fn main() {
    write(stdout(), to_bytes(format("%i", f(1))));
    write(stdout(), to_bytes(format("%i", f(1, 2))));
    write(stdout(), to_bytes(format("%i", f(1, 2, 3, 4))));
}
