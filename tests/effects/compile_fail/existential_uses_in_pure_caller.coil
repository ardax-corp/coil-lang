// @expected E0413 — a call through an existential of a trait whose method
// declares `uses {write}` has that effect, even if this impl does not.
trait Sink<S> {
    fn put(S self, int x) uses {write} {}
}

class Null {
    pub n: int,
}

impl Sink for Null {
    fn put(Null self, int x) {}
}

pure fn feed(Sink s) {
    put(s, 1);
}

fn main() {
    feed(new Null(0));
}
