// A user generic enum over a shared body's type parameter, past the clone
// budget (E0804).
// @flags --clone-budget 8
use tests::assert;

class A {
    pub v: int,
}

class B {
    pub v: int,
}

class C {
    pub v: int,
}

class D {
    pub v: int,
}

class E {
    pub v: int,
}

class F {
    pub v: int,
}

class G {
    pub v: int,
}

class H {
    pub v: int,
}

enum Slot<T> {
    Full(T),
    Empty,
}

fn slot<T>(T x, bool full) -> Slot<T> {
    if full {
        return Slot::Full(x);
    }
    return Slot::Empty;
}

fn fill() {
    let a = slot(new A(1), true);
    let b = slot(new B(1), true);
    let c = slot(new C(1), true);
    let d = slot(new D(1), true);
    let e = slot(new E(1), true);
    let f = slot(new F(1), true);
    let g = slot(new G(1), true);
    let h = slot(new H(1), true);
}

test("shared body user enum") {
    fill();
    let n = match slot(5, true) {
        Slot::Full(v) => v,
        Slot::Empty => -1,
    };
    assert!(n == 5);
    let e = match slot(5, false) {
        Slot::Full(v) => v,
        Slot::Empty => -1,
    };
    assert!(e == -1);
    let f = match slot(2.5, true) {
        Slot::Full(v) => v,
        Slot::Empty => 0.0,
    };
    assert!(f == 2.5);
    let s = match slot("hi", true) {
        Slot::Full(v) => v,
        Slot::Empty => "",
    };
    assert!(s == "hi");
}
