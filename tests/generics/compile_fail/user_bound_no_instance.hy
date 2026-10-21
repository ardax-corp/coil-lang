// Expected: E0119 — `float` has no `Size` instance, so it does not satisfy `T: Size`.
trait Size<T> {
    fn size(T x) -> int {}
}

impl Size for int {
    fn size(int x) -> int {
        return x;
    }
}

fn total<T: Size>(T a, T b) -> int {
    return size(a) + size(b);
}

fn main() {
    let n = total(1.5, 2.5);
}
