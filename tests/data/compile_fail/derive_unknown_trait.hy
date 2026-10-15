// Expected: E0119 — `Num` is not a derivable trait.
#[derive(Num)]
class C {
    pub v: int,
}

fn main() {
}
