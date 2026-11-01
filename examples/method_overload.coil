// Methods overloaded by arity.
//
// @output 1116

use io::sync::print;
use string::format;

class Counter {
    pub value: int,
}

impl Counter {
    pub fn bump(int by) -> int {
        self.value = self.value + by;
        return self.value;
    }

    pub fn bump() -> int {
        return self.bump(1);
    }
}

fn main() {
    let c = new Counter(10);
    let _ = print(format("%i", c.bump()));
    let _ = print(format("%i", c.bump(5)));
}
