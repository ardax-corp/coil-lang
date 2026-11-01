// Class statics updated from instance methods.
//
// @output 121

use io::sync::print;
use string::format;

static let hits = 0;

class Counter {
    pub static count: int = 0,
    pub value: int,
}

impl Counter {
    pub fn bump() {
        Counter::count = Counter::count + 1;
        self.value = self.value + 1;
    }
}

fn main() {
    hits = hits + 1;
    Counter::count = Counter::count + 1;
    let c = new Counter(0);
    c.bump();
    let _ = print(format("%i", hits));
    let _ = print(format("%i", Counter::count));
    let _ = print(format("%i", c.value));
}
