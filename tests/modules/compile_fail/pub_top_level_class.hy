// Expected: E0001 — `pub` is not allowed on a top-level `class`; `pub` marks class fields and `impl` methods only.
pub class Point {
    pub x: int,
}

fn main() {
    println(new Point(1).x);
}
