// @expected E0102 — the impl's `scale` takes a string, the trait declares int (#1185).
trait Scale<S> {
    fn scale(S self, int k) -> int {}
}

class Sq {
    pub side: int,
}

impl Scale for Sq {
    pub fn scale(Sq self, string k) -> int {
        return self.side;
    }
}

fn main() {
    let s = new Sq(2);
    let a = s.scale(3);
}
