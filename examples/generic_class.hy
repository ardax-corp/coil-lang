// examples/generic_class.hy — generic class + impl end-to-end.
//
// Output: 42

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
class Cell<T> {
    pub value: T
}

impl Cell<T> {
    pub fn get() -> T {
        return self.value;
    }
}

fn main() {
    let c = new Cell(42);
    write_all(stdout(), to_bytes(format("%i", c.get())));
}
