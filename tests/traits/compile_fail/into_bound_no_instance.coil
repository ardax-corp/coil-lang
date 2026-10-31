// @expected E0119 — `wrap` needs an `Into<int, B>` instance; none is declared.
fn wrap<A, B>(A x) -> B where Into<A, B> {
    return into(x);
}

fn main() {
    let w = wrap(42);
}
