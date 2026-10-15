// Class `const` fields are readable after construction (mutation is blocked separately).
// Output: 79
use io::{stdout, write};
use string::{format, to_bytes};

class Point {
    pub const x: int,
    pub y: int,
}

fn main() {
    let p = new Point(7, 9);
    write(stdout(), to_bytes(format("%i", p.x)));
    write(stdout(), to_bytes(format("%i", p.y)));
}
