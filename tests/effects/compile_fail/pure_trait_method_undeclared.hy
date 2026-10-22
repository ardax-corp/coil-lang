// @expected E0413 — a call through a trait method with no declaration has unknown
// effects, so a `pure fn` cannot make it.
trait Size<T> {
    fn size(T x) -> int {}
}

impl Size for int {
    pub fn size(int x) -> int {
        return x;
    }
}

pure fn total(Size a) -> int {
    return size(a);
}

fn main() {
    total(1);
}
