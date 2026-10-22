// @expected E0102 — a `string -> int` value does not fit a `fn(int) -> int` parameter.
fn apply(fn(int) -> int f) -> int {
    return f(1);
}

fn size(string s) -> int {
    return 1;
}

fn main() {
    let r = apply(size);
}
