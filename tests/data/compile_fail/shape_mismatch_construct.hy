// Expected: E0205 — record variant called as tuple.
enum Point {
    P { x: int, y: int },
    Q(int),
}

fn main() {
    let p = Point::P(1, 2);
}
