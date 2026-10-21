// A `let` bound from a `match` inside a `for … in` body (with a panicking
// arm that never runs) feeds an accumulator declared outside the loop.
// Output: 9
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let items = [1, 2, 3];
    let acc = 0;
    for n in items {
        let v = match Result::Ok(n) {
            Result::Ok(x) => x + 1,
            Result::Err(_) => panic "tick",
        };
        acc = acc + v;
    }
    let _ = write(stdout(), to_bytes(format("%i", acc)));
}
