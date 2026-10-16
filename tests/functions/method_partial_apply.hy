// Calling a method with fewer arguments than it declares makes a function
// value over the receiver and the given arguments (#1231).
use tests::assert;

class Foo {
    pub v: int,
}

impl Foo {
    pub fn bump(Foo f) -> int {
        return f.v + 1;
    }

    pub fn add3(self, int a, int b, int c) -> int {
        return self.v * 1000 + a * 100 + b * 10 + c;
    }

    pub fn label(self, string head) -> string {
        return head + "#" + "{self.v}";
    }
}

fn apply(int -> int f, int x) -> int {
    return f(x);
}

test("a method with no arguments given takes them later") {
    let f = new Foo(1);
    let g = new Foo(41);
    let bump = f.bump();
    assert!(bump(g) == 42);
    assert!(f.bump(g) == 42);
}

test("a prefix of a method's arguments") {
    let f = new Foo(2);
    let one = f.add3(3);
    assert!(one(4, 5) == 2345);
    let two = f.add3(6, 7);
    assert!(two(8) == 2678);
    assert!(apply(two, 9) == 2679);
}

test("the receiver is captured when the call is made") {
    let mut f = new Foo(5);
    let tag = f.label();
    f = new Foo(6);
    assert!(tag("x") == "x#5");
    assert!(f.label("y") == "y#6");
}
