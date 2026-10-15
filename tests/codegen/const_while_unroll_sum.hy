// IL unroll of counted `while` must still run the induction step each trip.
// Output: 6
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let s = 0;
    let i = 0;
    while i < 4 {
        s = s + i;
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", s)));
}
