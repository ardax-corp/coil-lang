// Output: n=77!
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let n = 7;
    let _ = write(stdout(), to_bytes("n=" + format("%i", n)));
    let _ = write(stdout(), to_bytes(format("%i", n) + "!"));
}
