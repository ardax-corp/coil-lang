// Expected: E0119 — `Num` is not a derivable trait.
#[derive(Num)]
class Sq {
    pub side: int,
}

fn main() {}
