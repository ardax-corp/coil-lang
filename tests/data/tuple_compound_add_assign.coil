// @output 46
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let v = (1, 2);
    v += (3, 4);
    let _ = write(stdout(), to_bytes(format("%i%i", v[0], v[1])));
}
