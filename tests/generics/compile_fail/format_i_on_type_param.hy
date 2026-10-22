// @expected E0300 — `%i` needs a concrete `int`; an open type parameter needs `%v`.
use string::format;

fn text<T: Show>(T x) -> string {
    return format("%i", x);
}

fn main() {
    let s = text(1);
}
