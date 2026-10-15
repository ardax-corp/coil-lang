// Range for-in unroll (`0..3`) binds successive values correctly.
// Output: 3
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let s = 0;
    for x in 0..3 {
        s = s + x;
    }
    write(stdout(), to_bytes(format("%i", s)));
}
