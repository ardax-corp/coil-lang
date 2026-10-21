// Expected: E0101 — a `comptime` block can only call code from other
// modules: this module is not compiled yet when the block runs.
use comptime_gen::adder;

fn base() -> int {
    return 1;
}

comptime {
    return adder("f", base());
}

fn main() {}
