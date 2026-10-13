// `x * 2^n` / `2^n * x` / `x * const(2^n)` must lower to SHL but still
// compute the same values as MUL (wrong shift amount is silent otherwise).
// Output: 40,28,48,9,42,-24
use io::{stdout, write};
use string::{format, to_bytes};

fn scale_rhs(int x) -> int {
    return x * 8;
}

fn scale_lhs(int x) -> int {
    return 4 * x;
}

fn scale_const(int x) -> int {
    const K = 16;
    return x * K;
}

fn scale_one(int x) -> int {
    return x * 1;
}

fn scale_six(int x) -> int {
    return x * 6;
}

fn scale_negative_x(int x) -> int {
    return x * 8;
}

fn main() {
    write(stdout(), to_bytes(format("%i,", scale_rhs(5))));
    write(stdout(), to_bytes(format("%i,", scale_lhs(7))));
    write(stdout(), to_bytes(format("%i,", scale_const(3))));
    write(stdout(), to_bytes(format("%i,", scale_one(9))));
    write(stdout(), to_bytes(format("%i,", scale_six(7))));
    write(stdout(), to_bytes(format("%i", scale_negative_x(0 - 3))));
}
