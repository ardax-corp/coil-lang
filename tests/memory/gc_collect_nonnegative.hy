// Output: true
use gc::collect;
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let n = collect();
    write(stdout(), to_bytes(format("%z", n >= 0)));
}
