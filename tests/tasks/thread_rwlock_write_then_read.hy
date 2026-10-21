// Output: 11
use thread::{rwlock, with_read, with_write};
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let rw = rwlock(10)?;
    with_write(rw, fn (int n) => (n + 1, 0))?;
    let v = with_read(rw, fn (int n) => n)?;
    let _ = write(stdout(), to_bytes(format("%i", v)));
}
