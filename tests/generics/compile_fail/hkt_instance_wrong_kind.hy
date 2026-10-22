// @expected E0119 — `Container<F: * -> *>` needs a type constructor; `int` has kind `*`.
trait Container<F: * -> *> {
    fn first<A>(F<A> xs, A fallback) -> A {}
}

impl Container for int {
    pub fn first<A>(int xs, A fallback) -> A {
        return fallback;
    }
}

fn main() {}
