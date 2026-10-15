// Expected: E0119 — `F` has kind `* -> *`, so `F<F>` passes a constructor where a
// type of kind `*` is needed.
trait Container<F: * -> *> {
    fn first<A>(F<A> xs) -> A {}
}

fn bad<F: Container, A>(F<F> xs) -> A {
    return first(xs);
}

fn main() {}
