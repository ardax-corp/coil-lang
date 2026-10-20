// Expected: E0413 — a call that may store an argument into a private
// object makes it reach shared state, so mutating it is `mutate` (#1648).
class Inner {
    pub n: int,
}

class Holder {
    pub inner: Inner,
}

impl Holder {
    pub fn set(Inner i) {
        self.inner = i;
    }

    pub fn poke() {
        self.inner.n = 5;
    }
}

pure fn poke_through(Inner i) {
    let h = new Holder(new Inner(0));
    h.set(i);
    h.poke();
}

fn main() {
    poke_through(new Inner(1));
}
