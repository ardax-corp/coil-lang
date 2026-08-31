// Expected: compile failure — cannot assign to const class field.
class Point {
    pub const x: int,
    pub y: int,
}

fn main() {
    let p = new Point(1, 2);
    p.x = 10;
}
