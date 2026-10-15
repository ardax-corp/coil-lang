// A `thread::spawn` worker writing to stdout in a loop prints every iteration
// before `join` returns.
// Output: 012
use thread::{join, spawn};
use io::{stdout, write};
use string::{format, to_bytes};

fn worker() -> int {
    let i = 0;
    while i < 3 {
        write(stdout(), to_bytes(format("%i", i)));
        i = i + 1;
    }
    return 0;
}

fn main() {
    let t = spawn(worker)?;
    join(t)?;
}
