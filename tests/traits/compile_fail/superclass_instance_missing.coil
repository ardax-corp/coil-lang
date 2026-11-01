// @expected E0119 — `impl Ordered for int` needs the superclass instance `Equal<int>`.
trait Equal<T> {
    fn eq_val(T a, T b) -> bool {}
}

trait Ordered<T: Equal> {
    fn lt_val(T a, T b) -> bool {}
}

impl Ordered for int {
    fn lt_val(int a, int b) -> bool {
        return a < b;
    }
}

fn main() {
    lt_val(1, 2);
}
