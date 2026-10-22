// @expected E0300 — `%v` on a tuple holding an unbounded `T` needs `T: Show`.
use string::format;

fn bad<T>(T x) -> string {
    return format("%v", (x, 1));
}

fn main() {
    let s = bad(1);
}
