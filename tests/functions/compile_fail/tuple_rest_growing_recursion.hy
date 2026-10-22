// @expected E0119 — every call of `grow` passes one more argument than it
// got, so it would need a new copy for every pack length (coil-lang#1281).
fn grow(... xs) -> int {
    return grow(1, ...xs);
}

fn main() {
    let n = grow(1);
}
