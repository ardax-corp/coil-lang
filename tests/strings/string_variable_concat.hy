// Output: GET/hi
use io::{stdout, write};
use string::to_bytes;

fn main() {
    let a = "GET";
    let b = "/hi";
    write(stdout(), to_bytes(a + b));
}
