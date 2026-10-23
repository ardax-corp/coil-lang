// Module statics (`static let`) and class statics.
//
// @output 11

use io::sync::print;
use string::format;

static let hits = 0;

class Counter {
    pub static count: int = 0,
    pub value: int,
}

fn main() {
    hits = hits + 1;
    Counter::count = Counter::count + 1;
    let _ = print(format("%i", hits));
    let _ = print(format("%i", Counter::count));
}
