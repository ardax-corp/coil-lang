// @expected E0102 — `string -> int` does not fit an `int -> int` parameter.
fn apply(int -> int f) -> int {
    return f(1);
}

fn size(string s) -> int {
    return 1;
}

fn main() {
    let r = apply(size);
}
