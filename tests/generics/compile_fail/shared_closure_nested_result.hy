// Expected: E0801 — past the clone cap (E0804) the shared body's closure
// holds its type parameter's value boxed inside the `Option` it returns,
// which no adapter turns into a ground `Option<string>`, so the call is
// refused rather than misread (#1532).
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

fn opt<T>(T x) -> unit -> Option<T> {
    return fn () use (x) => Some(x);
}

fn at_o(unit -> Option<string> f) -> string {
    return match f() {
        Some(v) => v,
        None => "none",
    };
}

fn main() {
    let a = opt(new A(1));
    let b = opt(new B(1));
    let c = opt(new C(1));
    let d = opt(new D(1));
    let e = opt(new E(1));
    let f = opt(new F(1));
    let g = opt(new G(1));
    let h = opt(new H(1));
    let s = at_o(opt("s"));
}
