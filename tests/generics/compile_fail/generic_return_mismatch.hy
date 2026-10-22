// @expected E0119 — a type parameter stands for any type, so returning `Some(1)` as
// `Option<T>` is an error.
fn wrap<T>(T x) -> Option<T> {
    return Option::Some(1);
}

fn main() {
    let o = wrap("a");
}
