// `e.f` on a record-shaped enum variant reads the payload field by its
// declared position, including through a nested variant and a
// single-field enum (which may travel as a two-word pair).
enum Inner {
    Inner { v: int },
}

enum Outer {
    Outer { x: Inner, y: int },
}

enum Shape {
    Dot,
    Box { w: int, h: int },
}

fn inner_v(Inner i) -> int {
    return i.v;
}

fn nested(Outer o) -> int {
    return o.x.v * 100 + o.y;
}

fn height(Shape s) -> int {
    return match s {
        Shape::Dot => 0,
        Shape::Box { w: _, h: _ } => s.h * 10 + s.w,
    };
}

test("record variant field reads") {
    assert(inner_v(Inner::Inner { v: 7 }) == 7)?;
    assert(nested(Outer::Outer { x: Inner::Inner { v: 3 }, y: 4 }) == 304)?;
    assert(height(Shape::Box { w: 2, h: 5 }) == 52)?;
    assert(height(Shape::Dot) == 0)?;
}
