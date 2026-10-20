// Expected: E0107 — no method may write a field of a readonly class, `self` included (#1558).
readonly class Point {
    pub x: int,
}

impl Point {
    pub fn bump() {
        self.x = self.x + 1;
    }
}

fn main() {
    new Point(1).bump();
}
