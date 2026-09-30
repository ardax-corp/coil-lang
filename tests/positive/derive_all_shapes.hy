// Every built-in derive on every declaration shape (classes, unit / tuple /
// record enums, scalar enums). Also the input of the derive equivalence gate.
use string::format;

#[derive(Show, Eq, Ord, Default, Hash, String, Send, Sensitive)]
class Pt {
    pub x: int,
    pub y: int,
}

#[derive(Show, Eq, Ord, Default, Hash, String)]
class Unit {}

#[derive(Show, Eq, Ord, Default, Hash, String, Send)]
enum Sh {
    Dot,
    Circle(int),
    Pair(int, int),
    Rect { w: int, h: int },
}

// Tuple variants of different payload types share payload index 0.
#[derive(Show, Eq)]
enum Holder {
    S(string),
    B(bool),
}

#[derive(Show, Eq, Ord, Hash, String, Default)]
enum Status {
    Ok = 200,
    NotFound = 404,
}

#[repr(string)]
#[derive(Show, Eq, Ord, Hash, String)]
enum Mode {
    Read = "r",
    Write = "w",
}

test("class Show / String / Eq") {
    let p = new Pt(1, 2);
    assert(p.show() == "Pt { x: 1, y: 2 }", p.show())?;
    assert(p.to_string() == "Pt { x: 1, y: 2 }", p.to_string())?;
    assert(format("%v", p) == "Pt { x: 1, y: 2 }", format("%v", p))?;
    assert(p == new Pt(1, 2), "eq")?;
    assert(p != new Pt(1, 3), "ne")?;
}

// `Default` is derived above (and typechecked) but has no call syntax yet.
test("class Ord / Hash") {
    assert(new Pt(1, 2) < new Pt(1, 3))?;
    assert(new Pt(1, 2) <= new Pt(1, 2))?;
    assert(new Pt(2, 0) > new Pt(1, 9))?;
    assert(new Pt(2, 0) >= new Pt(2, 0))?;
    assert(new Pt(1, 2).hash() == new Pt(1, 2).hash())?;
    assert(new Pt(1, 2).hash() != new Pt(2, 1).hash())?;
}

test("enum Show / String") {
    assert(Sh::Dot.show() == "Sh::Dot")?;
    assert(Sh::Circle(3).show() == "Sh::Circle(3)", Sh::Circle(3).show())?;
    assert(Sh::Pair(1, 2).show() == "Sh::Pair(1, 2)", Sh::Pair(1, 2).show())?;
    assert(Sh::Rect{ w: 1, h: 2 }.to_string() == "Sh::Rect { w: 1, h: 2 }")?;
}

test("enum Eq / Ord") {
    assert(Sh::Circle(3) == Sh::Circle(3))?;
    assert(Sh::Circle(3) != Sh::Circle(4))?;
    assert(Sh::Pair(1, 2) != Sh::Pair(1, 3))?;
    assert(Sh::Dot != Sh::Circle(0))?;
    assert(Sh::Dot < Sh::Circle(0))?;
    assert(Sh::Circle(1) < Sh::Circle(2))?;
    assert(Sh::Rect{ w: 1, h: 2 } < Sh::Rect{ w: 1, h: 3 })?;
    assert(Sh::Rect{ w: 1, h: 2 } >= Sh::Rect{ w: 1, h: 2 })?;
    assert(Sh::Rect{ w: 0, h: 0 } > Sh::Pair(9, 9))?;
}

test("enum Hash") {
    assert(Sh::Circle(3).hash() == Sh::Circle(3).hash())?;
    assert(Sh::Circle(3).hash() != Sh::Circle(4).hash())?;
    assert(Sh::Dot.hash() != Sh::Circle(0).hash())?;
}

test("scalar enums use their backing") {
    assert(Status::Ok.show() == "200")?;
    assert(Status::NotFound.to_string() == "404")?;
    assert(Status::Ok == Status::Ok)?;
    assert(Status::Ok < Status::NotFound)?;
    assert(Status::Ok.hash() == 200.hash())?;
    assert(Mode::Write.show() == "w")?;
    assert(Mode::Read < Mode::Write)?;
}

test("tuple variants with different payload types") {
    assert(format("%v", Holder::S("x")) == "Holder::S(x)", format("%v", Holder::S("x")))?;
    assert(Holder::B(true).show() == "Holder::B(true)")?;
    assert(Holder::S("x") == Holder::S("x"))?;
    assert(Holder::S("x") != Holder::B(false))?;
}
