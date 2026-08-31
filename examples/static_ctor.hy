// examples/static_ctor.hy — `static fn` constructors alongside positional `new`.
//
// Positional `new Class(...)` is unchanged. A `static fn new(...)` (or any
// other static method) is called as `Class::new(...)` and builds the
// instance by calling `new Class(...)` inside the body.
//
// Output: 42,1,1
//   42 — Point::new(40, 2).sum()
//   1  — Counter::fresh().id (count was bumped to 1)
//   1  — Counter::count after one fresh()

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub static fn new(int x, int y) -> Point {
        return new Point(x, y);
    }

    pub fn sum() -> int {
        return self.x + self.y;
    }
}

class Counter {
    pub static count: int = 0,
    pub id: int,
}

impl Counter {
    pub static fn fresh() -> Counter {
        Counter::count = Counter::count + 1;
        return new Counter(Counter::count);
    }
}

fn main() {
    let p = Point::new(40, 2);
    write_all(stdout(), to_bytes(format("%i,", p.sum())));

    let c = Counter::fresh();
    write_all(stdout(), to_bytes(format("%i,", c.id)));
    write_all(stdout(), to_bytes(format("%i", Counter::count)));
}
