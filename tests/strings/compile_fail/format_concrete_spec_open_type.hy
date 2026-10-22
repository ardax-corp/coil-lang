// @expected E0300 — a concrete specifier on an open type parameter; use `%v`.
use string::format;

fn f<T: Show>(T x) -> string {
    return format("%i", x);
}

fn main() {
    let s = f(1);
}
