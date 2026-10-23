// Hit bench for COI-269 MIR CSE: two DIVF of the same operands per trip.
// Stack-IL CSE/GVN refuse DIVF; dense specialize + MIR GVN keep one divide.
use io::sync::print;
use string::format;

fn hot(float scale, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        let a = xf / scale;
        let b = xf / scale;
        s = s + a * b;
        i = i + 1;
    }
    return s;
}

fn main() {
    let _ = print(format("%i", hot(3.0, 2000000) as int));
}
