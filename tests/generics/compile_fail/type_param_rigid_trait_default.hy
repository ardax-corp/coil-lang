// Expected: E0119 — a trait's type parameter is rigid in its default
// method bodies (#838). `x > 0` would make `T` an `int` while the shared
// default body still receives a boxed `T`.
trait Area<T> {
    fn area(T x) -> int {
        if x > 0 {
            return 1;
        }
        return 2;
    }
    fn other(T x) -> int {}
}

impl Area for int {
    pub fn other(int n) -> int {
        return n;
    }
}

fn main() {
    let a = area(0);
}
