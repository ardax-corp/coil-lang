// `let x = match …` inside `while` must not corrupt loop locals across iterations.
// Output: 3,3
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let i = 0;
    let acc = 0;
    while i < 3 {
        let v = match Result::Ok(i) {
            Result::Ok(x) => x,
            Result::Err(_) => panic "tick",
        };
        acc = acc + v;
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i,%i", acc, i)));
}
