// `!` on the result of a call whose return type is not known yet (a bare
// `... xs` function, typed per call shape, or a `fn` declared further down
// with no return type) takes it as `bool` (coil-lang#1429, #1435).
// Output: true true
use io::{stdout, write};
use string::to_bytes;

fn second(int skip, ... xs) {
    return xs[1];
}

fn main() {
    if !second(0, 1.5, true) {
        write(stdout(), to_bytes("false"));
    } else {
        write(stdout(), to_bytes("true"));
    }
    if !later() {
        write(stdout(), to_bytes(" false"));
    } else {
        write(stdout(), to_bytes(" true"));
    }
}

fn later() {
    return true;
}
