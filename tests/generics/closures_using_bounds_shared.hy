// A closure in a bounded generic function's shared body (past the clone
// budget, E0804) reads the bound's dictionary it captured (#1526).
// @flags --clone-budget 8
use tests::assert;
use string::format;

class A {
    pub v: int,
}

impl Lt for A {
    pub fn lt(A a, A b) -> bool {
        return a.v < b.v;
    }
}

impl Show for A {
    pub fn show(A self) -> string {
        return format("A%i", self.v);
    }
}

class B {
    pub v: int,
}

impl Lt for B {
    pub fn lt(B a, B b) -> bool {
        return a.v < b.v;
    }
}

impl Show for B {
    pub fn show(B self) -> string {
        return format("B%i", self.v);
    }
}

class C {
    pub v: int,
}

impl Lt for C {
    pub fn lt(C a, C b) -> bool {
        return a.v < b.v;
    }
}

impl Show for C {
    pub fn show(C self) -> string {
        return format("C%i", self.v);
    }
}

class D {
    pub v: int,
}

impl Lt for D {
    pub fn lt(D a, D b) -> bool {
        return a.v < b.v;
    }
}

impl Show for D {
    pub fn show(D self) -> string {
        return format("D%i", self.v);
    }
}

class E {
    pub v: int,
}

impl Lt for E {
    pub fn lt(E a, E b) -> bool {
        return a.v < b.v;
    }
}

impl Show for E {
    pub fn show(E self) -> string {
        return format("E%i", self.v);
    }
}

class F {
    pub v: int,
}

impl Lt for F {
    pub fn lt(F a, F b) -> bool {
        return a.v < b.v;
    }
}

impl Show for F {
    pub fn show(F self) -> string {
        return format("F%i", self.v);
    }
}

class G {
    pub v: int,
}

impl Lt for G {
    pub fn lt(G a, G b) -> bool {
        return a.v < b.v;
    }
}

impl Show for G {
    pub fn show(G self) -> string {
        return format("G%i", self.v);
    }
}

class H {
    pub v: int,
}

impl Lt for H {
    pub fn lt(H a, H b) -> bool {
        return a.v < b.v;
    }
}

impl Show for H {
    pub fn show(H self) -> string {
        return format("H%i", self.v);
    }
}

fn big<T: Lt>(T a, T b) -> T {
    let f = fn () use (a, b) {
        if a < b {
            return b;
        }
        a
    };
    return f();
}

fn pick<T: Lt>(T a, T b) -> T {
    let f = fn (T x, T y) {
        if x < y {
            return y;
        }
        x
    };
    return f(a, b);
}

fn shown<T: Show>(T x) -> string {
    let outer = fn () use (x) {
        let inner = fn () use (x) => format("[%v]", x);
        inner()
    };
    return outer();
}

test("bound operators in closures past the clone cap") {
    assert!(big(new A(1), new A(5)).v == 5);
    assert!(pick(new A(7), new A(2)).v == 7);
    assert!(shown(new A(3)) == "[A3]");
    assert!(big(new B(1), new B(5)).v == 5);
    assert!(pick(new B(7), new B(2)).v == 7);
    assert!(shown(new B(3)) == "[B3]");
    assert!(big(new C(1), new C(5)).v == 5);
    assert!(pick(new C(7), new C(2)).v == 7);
    assert!(shown(new C(3)) == "[C3]");
    assert!(big(new D(1), new D(5)).v == 5);
    assert!(pick(new D(7), new D(2)).v == 7);
    assert!(shown(new D(3)) == "[D3]");
    assert!(big(new E(1), new E(5)).v == 5);
    assert!(pick(new E(7), new E(2)).v == 7);
    assert!(shown(new E(3)) == "[E3]");
    assert!(big(new F(1), new F(5)).v == 5);
    assert!(pick(new F(7), new F(2)).v == 7);
    assert!(shown(new F(3)) == "[F3]");
    assert!(big(new G(1), new G(5)).v == 5);
    assert!(pick(new G(7), new G(2)).v == 7);
    assert!(shown(new G(3)) == "[G3]");
    assert!(big(new H(1), new H(5)).v == 5);
    assert!(pick(new H(7), new H(2)).v == 7);
    assert!(shown(new H(3)) == "[H3]");
    assert!(big(3, 9) == 9);
    assert!(pick(9, 3) == 9);
    assert!(big(1.5, -1.5) == 1.5);
    assert!(shown(4) == "[4]");
    assert!(shown("s") == "[s]");
}
