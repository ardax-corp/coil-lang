// Expected: E0119 — `Area::size("x")` needs an `Area<string>` instance.
trait Area<T> {
    fn size(T s) -> int {}
}

impl Area for int {
    pub fn size(int n) -> int {
        return n;
    }
}

fn main() {
    let a = Area::size("x");
}
