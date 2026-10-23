// examples/generic_class.hy — generic class + impl end-to-end.
//
// @output 42

use io::sync::print;
use string::format;

class Cell<T> {
    pub value: T,
}

impl Cell<T> {
    pub fn get() -> T {
        return self.value;
    }
}

fn main() {
    let c = new Cell(42);
    let _ = print(format("%i", c.get()));
}
