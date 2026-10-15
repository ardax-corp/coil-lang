// Expected: E0119 — `Bifunctor` takes a `* -> * -> *` constructor; `Option` has
// kind `* -> *`.
trait Bifunctor<F: * -> * -> *> {
    fn tag<A, B>(F<A, B> xs) -> int {}
}

impl Bifunctor for Option {
    pub fn tag<A, B>(Option<A> xs) -> int {
        return 0;
    }
}

fn main() {}
