// @expected E0300 — `%v` on a type parameter needs a `Show` bound.
use string::format;

fn text<T>(T x) -> string {
    return format("%v", x);
}

fn main() {
    let s = text(1);
}
