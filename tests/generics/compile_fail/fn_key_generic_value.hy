// @expected E0145 — a generic function value has no type arguments at run
// time, so a generic `fn_key()` user cannot be one.
use symbol::fn_key;

fn ident<T>(T x) -> int {
    return fn_key();
}

fn main() {
    let f = ident;
    let _ = f(3);
}
