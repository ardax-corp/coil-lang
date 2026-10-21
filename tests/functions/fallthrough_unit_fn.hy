// Output: ok
use io::{stdout, write};
use string::{format, to_bytes};

fn unitish() {
    let x = 1;
}

fn main() {
    unitish();
    let _ = write(stdout(), to_bytes("ok"));
}
