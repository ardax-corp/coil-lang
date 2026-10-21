// Expected: E0104 — `K::n(1)` calls a class static that is an `int`, not a function (#1726).
class K {
    pub static n: int = 1,
}

fn main() {
    K::n(1);
}
