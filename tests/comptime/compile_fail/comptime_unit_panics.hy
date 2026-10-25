// @expected E0134 — a unit `comptime` call panics while compiling (#1798).
fn boom() {
    panic "boom";
}

fn main() {
    comptime boom();
}
