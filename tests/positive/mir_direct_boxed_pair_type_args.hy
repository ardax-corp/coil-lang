// A two-word enum (`[payload, tag]` pair) passed as a shared generic's
// bare type parameter goes boxed to its enum object and then to its type
// tag, as HIR emission passes it, and the result unboxes back to that
// object. Direct MIR makes the same call.
use tests::assert;

enum Tagged {
    Num(int),
    Empty,
}

impl Tagged {
    pub fn num() -> int {
        return match self {
            Tagged::Num(n) => n,
            Tagged::Empty => -1,
        };
    }
}

fn make(int n) -> Tagged {
    if n < 0 {
        return Tagged::Empty;
    }
    return Tagged::Num(n);
}

fn ident<T>(T x) -> T {
    return x;
}

fn second<T>(T a, T b) -> T {
    return b;
}

test("a pair enum through a bare type parameter") {
    assert!(ident(make(7)).num() == 7);
}

test("two pair enums through a bare type parameter") {
    assert!(second(make(4), make(-5)).num() == -1);
}
