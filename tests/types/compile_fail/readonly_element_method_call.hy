// Expected: E0107 — `xs[0].bump()` mutates `self`, so it cannot be called on an element of a `readonly` array (#1552).
class Point {
    pub x: int,
}

impl Point {
    pub fn bump() {
        self.x = self.x + 1;
    }
}

fn main() {
    let xs = readonly [new Point(1)];
    xs[0].bump();
}
