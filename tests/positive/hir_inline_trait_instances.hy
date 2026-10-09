// Typed inlining splices a ground trait instance's method body at the call:
// scalar and class receivers, locals in the body, and a default method that
// stays a call (its body is shared by every instance).

class Point {
    pub x: int,
    pub y: int,
}

trait Measure<S> {
    fn size(S s, int k) -> int {}

    fn twice(S s, int k) -> int {
        return s.size(k) * 2;
    }
}

impl Measure for int {
    pub fn size(int s, int k) -> int {
        let t = s * k;
        return t + 1;
    }
}

impl Measure for Point {
    pub fn size(Point p, int k) -> int {
        let t = p.x * k;
        return t + p.y;
    }
}

fn hot(int n) -> int {
    let p = new Point(3, 4);
    let acc = 0;
    let i = 0;
    while i < n {
        acc = acc + i.size(3) + p.size(i) + i.twice(2);
        i = i + 1;
    }
    return acc;
}

fn shadow(int t) -> int {
    // The spliced `t` must not clobber the caller's `t`.
    let s = t.size(2);
    return s + t;
}

test("instance methods inline in a loop") {
    // i.size(3) = 3i+1, p.size(i) = 3i+4, i.twice(2) = 2(2i+1)
    // sum over i in 0..4 of 10i + 7 = 60 + 28
    assert(hot(4) == 88)?;
}

test("spliced locals stay apart from the caller's") {
    assert(shadow(5) == 16)?;
}
