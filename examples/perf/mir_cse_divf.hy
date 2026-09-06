// Hit bench for COI-269 MIR CSE: two DIVF of the same operands per trip.
// Stack-IL CSE/GVN refuse DIVF; dense specialize + MIR GVN keep one divide.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn hot(float scale, int n) -> int {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        let a = xf / scale;
        let b = xf / scale;
        s = s + a * b;
        i = i + 1;
    }
    return s as int;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", hot(3.0, 2000000))));
}
