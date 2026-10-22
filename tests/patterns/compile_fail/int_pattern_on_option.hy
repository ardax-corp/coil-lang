// @expected E0102 — an integer literal pattern needs an `int` scrutinee.
fn f(Option<int> o) -> int {
    return match o {
        1 => 1,
        default => 0,
    };
}

fn main() {
    let r = f(Option::None);
}
