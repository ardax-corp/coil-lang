// `%v` of a tuple or anonymous record holding an enum value shows each
// item with the enum's own `Show` (#1176).
use tests::assert;
use string::format;

#[derive(Show)]
enum Tag {
    Plain,
    Named(string),
}

#[derive(Show)]
enum Color {
    Red,
    Green,
}

enum Opaque {
    One,
    Two(int),
}

test("tuple holding a payload enum") {
    let s = format("%v", (Tag::Named("n"), 1));
    assert!(s == "(Tag::Named(n), 1)", s);
}

test("tuple holding a unit variant of a payload enum") {
    let s = format("%v", (1, Tag::Plain));
    assert!(s == "(1, Tag::Plain)", s);
}

test("tuple holding a scalar enum") {
    let s = format("%v", (Color::Green, "x"));
    assert!(s == "(Color::Green, x)", s);
}

test("record holding an enum") {
    let s = format("%v", { p: Tag::Plain });
    assert!(s == "{ p: Tag::Plain }", s);
}

test("tuple local holding an enum") {
    let t = (Tag::Named("a"), Color::Red);
    let s = format("%v", t);
    assert!(s == "(Tag::Named(a), Color::Red)", s);
}

test("enum with the default Show inside a tuple") {
    let s = format("%v", (Opaque::Two(3), 0));
    assert!(len(s) > 0, s);
}
