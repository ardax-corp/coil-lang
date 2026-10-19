// Expected: E0119 — a method's own type parameter is not its impl's (#1457).
class Cell<T> {
    pub value: T,
}

impl Cell<T> {
    pub fn swap<U>(U other) -> T {
        return other;
    }
}

fn main() {}
