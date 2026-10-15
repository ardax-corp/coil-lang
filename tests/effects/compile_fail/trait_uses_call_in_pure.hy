// Expected: E0413 — a call through a trait method declared `uses {read}` has `read`.
trait Load<A> {
    fn load(A self) -> int uses {read} {}
}

class Src {
    pub v: int,
}

impl Load for Src {
    fn load(Src self) -> int {
        return self.v;
    }
}

pure fn total(Load a) -> int {
    return load(a);
}

fn main() {
    total(new Src(1));
}
