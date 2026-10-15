// `%v` on nested values: tuples in tuples, one-tuples, dicts in dicts
// (fields in name order), derived class Show inside aggregates, an enum
// field of a class, and values shown through a `T: Show` bound.
use tests::assert;
use string::format;

#[derive(Show)]
class Pt {
    pub x: int,
    pub y: int,
}

#[derive(Show)]
enum Tag {
    Plain,
    Named(string),
}

#[derive(Show)]
class Line {
    pub a: Pt,
    pub tag: Tag,
}

fn shown<T: Show>(T x) -> string {
    return format("<%v>", x);
}

test("tuples nest and keep their element formats") {
    let t = format("%v", (1, (2.5, "s"), true));
    assert!(t == "(1, (2.5, s), true)", t);
    assert!(format("%v", ("x",)) == "(x,)", format("%v", ("x",)));
    assert!(format("%v", ((1,), (2, 3))) == "((1,), (2, 3))");
}

test("dicts nest with fields in name order") {
    let d = format("%v", { b: (1, 2), a: { d: 1, c: 2 } });
    assert!(d == "{ a: { c: 2, d: 1 }, b: (1, 2) }", d);
}

test("derived Show nests inside classes and tuples") {
    assert!(format("%v", new Pt(1, -2)) == "Pt { x: 1, y: -2 }", format("%v", new Pt(1, -2)));
    let t = format("%v", (new Pt(0, 0), 3));
    assert!(t == "(Pt { x: 0, y: 0 }, 3)", t);
    let l = format("%v", new Line(new Pt(1, 2), Tag::Named("n")));
    assert!(l == "Line { a: Pt { x: 1, y: 2 }, tag: Tag::Named(n) }", l);
    let d = format("%v", { p: new Pt(5, 6) });
    assert!(d == "{ p: Pt { x: 5, y: 6 } }", d);
}

test("through a Show bound") {
    assert!(shown(5) == "<5>");
    assert!(shown("q") == "<q>");
    assert!(shown(new Pt(3, 4)) == "<Pt { x: 3, y: 4 }>");
    assert!(shown(Tag::Named("z")) == "<Tag::Named(z)>");
}
