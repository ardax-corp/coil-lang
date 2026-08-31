// Expected: parse failure — `readonly` is a prefix (`readonly new C(...)`).
class Point {
    pub x: int,
    pub y: int,
}

fn main() {
    let p = new readonly Point(1, 2);
}
