// A method on a generic enum matches on `self`: the shared body reads `self`
// as `Opt<T>`, so the match sees the payloads instead of refusing with
// E0801 (match-type).
use tests::assert;

enum Opt<T> {
    None,
    Some(T),
}

impl Opt<T> {
    pub fn is_some() -> bool {
        return match self {
            Opt::Some(_) => true,
            Opt::None => false,
        };
    }

    pub fn unwrap_or(T fallback) -> T {
        return match self {
            Opt::Some(v) => v,
            Opt::None => fallback,
        };
    }

    pub fn map<U>(T -> U f) -> Opt<U> {
        return match self {
            Opt::Some(v) => Opt::Some(f(v)),
            Opt::None => Opt::None,
        };
    }
}

class Box {
    pub n: int,
}

test("generic enum methods match on self") {
    let a: Opt<int> = Opt::Some(4);
    let none: Opt<int> = Opt::None;
    assert!(a.is_some());
    assert!(!none.is_some());
    assert!(a.unwrap_or(0) == 4);
    assert!(none.unwrap_or(7) == 7);
    assert!(a.map(fn (int x) => x * 10).unwrap_or(0) == 40);
    assert!(a.map(fn (int x) => new Box(x)).unwrap_or(new Box(0)).n == 4);
    let s: Opt<string> = Opt::Some("hi");
    assert!(s.map(fn (string t) => t.len()).unwrap_or(0) == 2);
}
