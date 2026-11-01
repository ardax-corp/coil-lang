// @expected E0407 — a gated call inside a method `main` calls is reached.
use env::exit;

class Stopper {
    pub code: int,
}

impl Stopper {
    pub fn go() {
        exit(self.code);
    }
}

fn main() {
    let s = new Stopper(3);
    s.go();
}
