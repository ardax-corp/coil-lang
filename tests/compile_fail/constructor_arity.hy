// Expected: E0204 — constructor arity mismatch.
// Avoid Some/None: those collide with prelude Option.
enum MyOpt {
    Yea(int),
    Nada,
}

fn main() {
    let o = MyOpt::Yea(1, 2);
}
