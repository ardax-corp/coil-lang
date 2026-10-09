// `arbitrary`: seeded random values for property tests, and
// `#[derive(Arbitrary)]` for classes and enums (contracts step C3).
use arbitrary::{Arbitrary, Gen, any};

#[derive(Arbitrary)]
class Point {
    pub x: int,
    pub y: int,
}

#[derive(Arbitrary)]
class Bag {
    pub names: Vec<string>,
    pub best: Option<Point>,
    pub ratio: float,
    pub on: bool,
}

// Recursive: derived values must end.
#[derive(Arbitrary)]
enum List {
    Cons(int, List),
    Nil,
}

#[derive(Arbitrary)]
enum Shape {
    Circle { r: int },
    Square(int),
    Dot,
}

#[derive(Arbitrary)]
class Pair<A, B> {
    pub a: A,
    pub b: B,
}

fn length(List l) -> int {
    return match l {
        List::Nil => 0,
        List::Cons(_, rest) => 1 + length(rest),
    };
}

test("the same seed gives the same values") {
    let g1 = Gen::new(99);
    let g2 = Gen::new(99);
    let i = 0;
    while i < 50 {
        let a: Point = any(g1);
        let b: Point = any(g2);
        assert(a.x == b.x && a.y == b.y)?;
        i += 1;
    }
}

test("ints stay within the size") {
    let g = Gen::new(3);
    g.resize(5);
    let i = 0;
    while i < 200 {
        let n: int = any(g);
        assert(n >= -5 && n <= 5, string::format("%i", n))?;
        i += 1;
    }
}

test("collections grow with the size") {
    let g = Gen::new(11);
    g.resize(0);
    let empty: Vec<int> = any(g);
    assert(len(empty) == 0)?;
    g.resize(30);
    let longest = 0;
    let i = 0;
    while i < 50 {
        let b: Bag = any(g);
        assert(len(b.names) <= 30)?;
        if len(b.names) > longest {
            longest = len(b.names);
        }
        i += 1;
    }
    assert(longest > 5, string::format("%i", longest))?;
}

test("recursive enums end") {
    let g = Gen::new(5);
    g.resize(100);
    let i = 0;
    while i < 100 {
        let l: List = any(g);
        assert(length(l) <= 5, string::format("%i", length(l)))?;
        i += 1;
    }
    assert(g.depth() == 0)?;
}

test("every variant shows up") {
    let g = Gen::new(8);
    let circles = 0;
    let squares = 0;
    let dots = 0;
    let i = 0;
    while i < 100 {
        let s: Shape = any(g);
        match s {
            Shape::Circle{ r: _ } => circles += 1,
            Shape::Square(_) => squares += 1,
            Shape::Dot => dots += 1,
        }
        i += 1;
    }
    assert(circles > 0 && squares > 0 && dots > 0)?;
}

test("generic classes take their parameters' instances") {
    let g = Gen::new(21);
    let i = 0;
    let trues = 0;
    while i < 40 {
        let p: Pair<bool, string> = any(g);
        if p.a {
            trues += 1;
        }
        i += 1;
    }
    assert(trues > 0 && trues < 40)?;
}

test("strings are valid text") {
    let g = Gen::new(4);
    g.resize(20);
    let i = 0;
    while i < 50 {
        let s: string = any(g);
        let b: byte = any(g);
        assert(len(s) <= 40)?;
        i += 1;
    }
}
