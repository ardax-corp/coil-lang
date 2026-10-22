// @expected E0119 — `Eq` is a prelude trait; declaring another needs
// `use prelude::ops::Eq as …;` first.
trait Eq<T> {
    fn same(T self, T other) -> bool {}
}

fn main() {}
