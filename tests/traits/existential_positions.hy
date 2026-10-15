// A unary trait name is an existential value type in a parameter, a return
// type and a `let` annotation. Each value keeps its own instance, so one
// function can return values of different types.
use tests::assert;

trait Describe<D> {
    fn describe(D self) -> string {}
}

#[derive(Show)]
class Pt {
    pub x: int,
    pub y: int,
}

class Name {
    pub s: string,
}

impl Describe for int {
    fn describe(int n) -> string {
        return "int " + show(n);
    }
}

impl Describe for Pt {
    fn describe(Pt p) -> string {
        return "pt " + show(p.x) + "," + show(p.y);
    }
}

impl Describe for Name {
    fn describe(Name n) -> string {
        return "name " + n.s;
    }
}

fn pick(int k) -> Describe {
    if k == 0 {
        return 7;
    }
    if k == 1 {
        return new Pt(1, 2);
    }
    return new Name("z");
}

fn both(Describe a, Describe b) -> string {
    return describe(a) + "/" + b.describe();
}

fn relay(Describe d) -> Describe {
    return d;
}

test("a let annotation packs a class value") {
    let p: Describe = new Pt(3, 4);
    assert!(describe(p) == "pt 3,4");
    let s: Show = new Pt(3, 4);
    assert!(show(s) == "Pt { x: 3, y: 4 }", show(s));
}

test("a return type packs each branch's value") {
    assert!(describe(pick(0)) == "int 7");
    assert!(describe(pick(1)) == "pt 1,2");
    assert!(describe(pick(2)) == "name z");
}

test("parameters of the same trait hold different types") {
    assert!(both(1, new Name("a")) == "int 1/name a");
    assert!(both(new Pt(0, 0), 2) == "pt 0,0/int 2");
}

test("an existential passes through unchanged") {
    assert!(describe(relay(pick(1))) == "pt 1,2");
    assert!(relay(4).describe() == "int 4");
}
