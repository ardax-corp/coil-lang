// @expected E0119 — the `<…>` of an inherent impl declares type parameters;
// `int` is a type, so `impl Cell<int>` is written `impl Cell<T>`.
class Cell<T> {
    pub v: T,
}

impl Cell<int> {
    pub fn get() -> int {
        return self.v;
    }
}

fn main() {}
