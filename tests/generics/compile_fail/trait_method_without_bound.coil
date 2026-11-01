// @expected E0119 — calling `size` on `T` needs a `T: Size` bound.
trait Size<T> {
    fn size(T x) -> int {}
}

impl Size for int {
    fn size(int x) -> int {
        return x;
    }
}

fn total<T>(T a) -> int {
    return size(a);
}

fn main() {
    let n = total(1);
}
