// P0: early-loop flag reassignment sticks while later locals stay live.
// Output: 16
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let got = 0;
    let a = 1;
    let b = 2;
    let c = 3;
    let i = 0;
    while i < 3 {
        got = 1;
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", got)));
    write(stdout(), to_bytes(format("%i", a + b + c)));
}
