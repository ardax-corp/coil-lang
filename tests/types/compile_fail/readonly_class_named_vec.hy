// @expected E0200 — `Vec` is the built-in `Vec<T>`; a readonly class cannot
// redeclare it either (#1654).
readonly class Vec {
    pub x: int,
    pub y: int,
}

fn sum(Vec v) -> int {
    return v.x + v.y;
}

fn main() {
    let _ = sum(new Vec(1, 2));
}
