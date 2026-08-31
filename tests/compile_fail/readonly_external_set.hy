// Expected: compile failure — external mutation of readonly value.
class Point {
    pub x: int,
    pub y: int,
}

fn main() {
    let p = readonly new Point(1, 2);
    p.x = 10;
}
