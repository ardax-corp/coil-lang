// @expected E0300 — `%v` on a type parameter needs a `Show` bound.
use string::format;

fn f<T>(T x) -> string {
    return format("%v", x);
}

fn main() {
    let s = f(1);
}
