// Output: 11,22,33,44
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let m = matrix([[1, 2], [3, 4]]);
    m += matrix([[10, 20], [30, 40]]);
    let _ = write(stdout(), to_bytes(format("%i,%i,%i,%i", m[0][0], m[0][1], m[1][0], m[1][1])));
}
