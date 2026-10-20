// Expected: E0200 — `Weak` is the built-in `gc::Weak<T>`, in scope with or
// without `use gc`; a class cannot redeclare it (#1654).
class Weak {
    pub n: int,
}

fn get(Weak w) -> int {
    return w.n;
}

fn main() {
    let _ = get(new Weak(3));
}
