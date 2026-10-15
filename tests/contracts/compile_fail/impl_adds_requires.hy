// Expected: E0119 — an impl of a trait method may add `ensures` but not `requires`.
trait Area<T> {
    fn area(T x) -> int
        ensures result >= 0
    {}
}

impl Area for int {
    pub fn area(int n) -> int
        requires n > 0
    {
        return n * n;
    }
}

fn main() {
    area(2);
}
