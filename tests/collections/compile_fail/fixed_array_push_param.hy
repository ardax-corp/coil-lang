// @expected E0412 — a `[T; N]` parameter cannot grow either; `push` needs a `Vec`.
fn bump([int; 2] xs) {
    xs.push(1);
}

fn main() {
    bump([0, 0]);
}
