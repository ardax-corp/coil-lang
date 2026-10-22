// @expected E0119 — a trait name is declared once per module.
trait Tiny<T> {
    fn id(T x) -> T {}
}

trait Tiny<T> {
    fn id(T x) -> T {}
}

fn main() {}
