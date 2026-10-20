// Expected: E0200 — `Range` is the built-in `Range<T>`; a class cannot
// redeclare it (#1654). Before, `Range` in type position named this class
// while `new Range(2, 7)` built the built-in `Range<a>`.
class Range {
    pub lo: int,
    pub hi: int,
}

fn width(Range r) -> int {
    return r.hi - r.lo;
}

fn main() {
    let r = new Range(2, 7);
    let _ = width(r);
}
