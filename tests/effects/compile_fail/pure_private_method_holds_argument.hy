// Expected: E0413 — an object made around an argument reaches it, so a
// method that mutates through it is `mutate` (#1648).
class Inner {
    pub n: int,
}

class Holder {
    pub inner: Inner,
}

impl Holder {
    pub fn poke() {
        self.inner.n = 5;
    }
}

pure fn poke_through(Inner i) {
    let h = new Holder(i);
    h.poke();
}

fn main() {
    poke_through(new Inner(1));
}
