// Expected: E0101 — a static trait method is called on its owner (`int::make()`);
// outside a trait body there is no bare spelling.
trait Make<T> {
    static fn make() -> T {}
}

impl Make for int {
    pub static fn make() -> int {
        return 1;
    }
}

fn main() {
    let x: int = make();
}
