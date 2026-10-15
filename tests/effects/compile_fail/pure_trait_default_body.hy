// Expected: E0413 — a trait method's default body must keep to the method's
// own `pure` declaration.
static let N: int = 0;

trait Count<A> {
    pure fn count(A self) -> int {
        N = N + 1;
        return N;
    }
}

class C {
    pub v: int,
}

impl Count for C {}

fn main() {
    count(new C(1));
}
