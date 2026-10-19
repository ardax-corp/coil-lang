// Expected: E0119 — a trait default's own type parameter is not the trait's (#1457).
trait Holder<T> {
    fn hold<U>(T self, U other) -> T {
        return other;
    }
}

fn main() {}
