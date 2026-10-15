// Expected: E0102 — `-` on a class with no `Sub` instance (#554).
class Vec2 {
    pub x: int,
}

fn main() {
    let d = new Vec2(1) - new Vec2(2);
}
