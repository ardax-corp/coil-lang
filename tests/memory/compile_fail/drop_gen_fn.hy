// Expected: E0126 — `fn drop` cannot be a `gen fn` (it must return unit).
class C {
    pub n: int,
}

impl C {
    gen fn drop() {
        yield 1;
    }
}

fn main() {}
