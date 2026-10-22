// `x = match …` reassignment inside a loop uses the same binding-context
// match lowering as `let x = match …`.
// @output 2
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let i = 0;
    let v = 0;
    while i < 3 {
        v = match Result::Ok(i) {
            Result::Ok(x) => x,
            Result::Err(_) => panic "tick",
        };
        i = i + 1;
    }
    let _ = write(stdout(), to_bytes(format("%i", v)));
}
