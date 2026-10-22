// @expected E0119 — a class with no `Integral` instance does not satisfy `T: Integral`.
class Flags {
    pub bits: int,
}

fn low<T: Integral>(T x, T m) -> T {
    return x & m;
}

fn main() {
    let f = low(new Flags(1), new Flags(2));
}
