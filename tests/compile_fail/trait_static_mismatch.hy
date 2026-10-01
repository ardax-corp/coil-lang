// Expected: E0119 — the trait declares `from_val` static, the
// instance implements it as an instance method.
class Val {
    pub i: int,
}

class Point {
    pub x: int,
}

trait FromVal<T> {
    static fn from_val(Val v) -> T {}
}

impl FromVal for Point {
    pub fn from_val(Val v) -> Point {
        return new Point(v.i);
    }
}

fn main() {}
