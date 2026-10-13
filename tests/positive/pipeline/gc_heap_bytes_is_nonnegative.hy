// Output: true
use gc::{heap_bytes, root};
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let _ = root([1, 2, 3, 4]);
    let n = heap_bytes();
    write(stdout(), to_bytes(format("%z", n >= 0)));
}
