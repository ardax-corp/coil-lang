// Expected: E0119 — `Container<F: * -> *>` needs a bare type constructor
// (`Option`), not an applied type (`Option<int>`).
trait Container<F: * -> *> {
    fn first<A>(F<A> xs) -> A {}
}

impl Container for Option<int> {
    pub fn first<A>(Option<A> xs) -> A {
        return match xs {
            Option::Some(v) => v,
            Option::None => 0,
        };
    }
}

fn main() {}
