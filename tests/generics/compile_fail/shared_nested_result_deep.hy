// @expected E0801 — past the clone cap (E0804) the shared body holds its
// type parameter's value boxed inside the tuple inside the `Option` it
// returns, which the call does not rebuild, so the call is refused rather
// than misread (#1549).
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

fn deep<T>(T x) -> Option<(T, int)> {
    return Some((x, 1));
}

fn main() {
    let a = deep(new A(1));
    let b = deep(new B(1));
    let c = deep(new C(1));
    let d = deep(new D(1));
    let e = deep(new E(1));
    let f = deep(new F(1));
    let g = deep(new G(1));
    let h = deep(new H(1));
    let t = match deep(5) {
        Some(p) => p,
        None => (0, 0),
    };
}
