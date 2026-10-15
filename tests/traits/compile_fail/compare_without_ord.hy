// Expected: E0102 — `<` on a class with no `Lt` instance.
class Sq {
    pub side: int,
}

fn main() {
    let b = new Sq(1) < new Sq(2);
}
