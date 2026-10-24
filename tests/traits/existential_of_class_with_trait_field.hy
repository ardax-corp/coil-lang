// A class holding a trait-typed field (or a `Vec` of them) packed into a
// trait value itself: the composite pattern (#1988).
use tests::assert;

trait Comp<T> {
    fn name(T c) -> string {}
}

class Label {
    pub text: string,
}

impl Comp for Label {
    fn name(Label l) -> string {
        return l.text;
    }
}

class Wrap {
    pub inner: Comp,
}

impl Comp for Wrap {
    fn name(Wrap w) -> string {
        return "[" + w.inner.name() + "]";
    }
}

class Group {
    pub kids: Vec<Comp>,
}

impl Comp for Group {
    fn name(Group g) -> string {
        let out = "";
        for k in g.kids {
            out = out + k.name();
        }
        return "(" + out + ")";
    }
}

test("a trait value of a class with a trait field") {
    let root: Comp = new Wrap(new Label("a"));
    assert!(root.name() == "[a]");
    let nested: Comp = new Wrap(new Wrap(new Label("b")));
    assert!(nested.name() == "[[b]]");
}

test("a trait value of a class with a Vec of trait values") {
    let kids: Vec<Comp> = Vec::new();
    kids.push(new Label("a"));
    kids.push(new Wrap(new Label("b")));
    let root: Comp = new Group(kids);
    assert!(root.name() == "(a[b])");
}
