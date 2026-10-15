// `coil test` adds a `contract: name` case for each function and `pub
// static` method with a `requires` or `ensures` whose parameters all have
// an `Arbitrary` instance (primitives, `Vec` / `Option` of them, derived
// types), calling it with random arguments that satisfy its `requires`.
// Every function here keeps its contract, so those cases pass.
use tests::assert;
use arbitrary::Arbitrary;

#[derive(Arbitrary)]
class Span {
    pub lo: int,
    pub len: int,
}

#[derive(Arbitrary)]
enum Shape {
    Dot,
    Line(int),
    Box { w: int, h: int },
}

fn clamp(int x, int lo, int hi) -> int
    requires lo <= hi
    ensures result >= lo && result <= hi
{
    if x < lo {
        return lo;
    }
    if x > hi {
        return hi;
    }
    return x;
}

fn total(Vec<int> xs) -> int
    requires len(xs) <= 1000
    ensures len(xs) > 0 || result == 0
{
    let s = 0;
    for x in xs {
        s = s + x;
    }
    return s;
}

fn or_zero(Option<int> o) -> int
    ensures match o {
        Option::Some(v) => result == v,
        Option::None => result == 0,
    }
{
    return o ?? 0;
}

fn either(bool a, bool b) -> bool
    ensures result == (a || b)
{
    if a {
        return true;
    }
    return b;
}

fn width(string s) -> int
    ensures result >= 0
{
    return len(s);
}

fn span_end(Span s) -> int
    requires s.len >= 0
    ensures result >= s.lo
{
    return s.lo + s.len;
}

fn corners(Shape s) -> int
    ensures result == 0 || result == 2 || result == 4
{
    return match s {
        Shape::Dot => 0,
        Shape::Line(_) => 2,
        Shape::Box { w: _, h: _ } => 4,
    };
}

class Grid {
    pub n: int,
}

impl Grid {
    pub static fn square(int n) -> Grid
        requires n >= 0 && n < 1000
        ensures result.n == n * n
    {
        return new Grid(n * n);
    }
}

test("the functions also work when called by hand") {
    assert!(clamp(5, 0, 3) == 3);
    assert!(total(Vec::from([1, 2, 3])) == 6);
    assert!(or_zero(Option::Some(4)) == 4);
    assert!(or_zero(Option::None) == 0);
    assert!(either(false, true));
    assert!(width("abc") == 3);
    assert!(span_end(new Span(2, 3)) == 5);
    assert!(corners(Shape::Box { w: 1, h: 2 }) == 4);
    assert!(Grid::square(3).n == 9);
}
