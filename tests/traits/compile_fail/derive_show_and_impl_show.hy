// Expected: E0119 — `#[derive(Show)]` and a hand-written `impl Show` for one type overlap.
#[derive(Show)]
class Sq {
    pub side: int,
}

impl Show for Sq {
    fn show(Sq s) -> string {
        return "sq";
    }
}

fn main() {
    show(new Sq(1));
}
