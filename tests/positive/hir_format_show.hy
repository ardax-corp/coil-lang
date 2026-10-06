// `%v` arguments at ground types: each value is shown through its `Show`
// instance (a user impl, a derived one, or a builtin) before `FORMAT`.
use string::format;

#[derive(Show)]
enum Shape {
    Circle(int),
    Square { side: int },
}

class Pt {
    pub x: int,
    pub y: int,
}

impl Show for Pt {
    fn show(Pt p) -> string {
        return format("<%i %i>", p.x, p.y);
    }
}

fn show_it<T: Show>(T x) -> string {
    return format("[%v]", x);
}

fn describe(Shape s, Pt p, int n) -> string {
    return format("%v %v %i %v", s, p, n, n + 1);
}

test("user, derived and builtin Show") {
    let p = new Pt(1, 2);
    assert(describe(Shape::Circle(3), p, 4) == "Shape::Circle(3) <1 2> 4 5")?;
    assert(format("%v", Shape::Square{ side: 2 }) == "Shape::Square { side: 2 }")?;
}

test("Show inside a call's arguments") {
    assert(len(format("%v-%v", true, Shape::Circle(1))) == 21)?;
}

test("Show through a generic mono instance") {
    assert(show_it(42) == "[42]")?;
    assert(show_it("hi") == "[hi]")?;
}
