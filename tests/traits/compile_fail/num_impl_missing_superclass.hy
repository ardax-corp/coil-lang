// Expected: E0119 — `impl Num` needs every superclass instance (`Sub`, … `Rem`, `Pow`).
class M {
    pub c: int,
}

impl Add for M {
    pub fn add(M a, M b) -> M {
        return new M(a.c + b.c);
    }
}

impl Num for M {}

fn main() {
    let m = new M(1) + new M(2);
}
