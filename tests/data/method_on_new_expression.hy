// Regression: method call on `(new Class(...))` must resolve the owner.
// Output: 4
use io::{stdout, write};
use string::{format, to_bytes};

class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub fn sum() -> int {
        return self.x + self.y;
    }
}

fn main() {
    let _ = write(stdout(), to_bytes(format("%i", (new Point(1, 3)).sum())));
}
