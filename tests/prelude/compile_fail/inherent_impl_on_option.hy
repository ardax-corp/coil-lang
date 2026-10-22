// @expected E0119 — inherent methods on `Option` live in the core prelude;
// another module adds methods through a trait.
impl Option<T> {
    pub fn twice() -> int {
        return 2;
    }
}

fn main() {}
