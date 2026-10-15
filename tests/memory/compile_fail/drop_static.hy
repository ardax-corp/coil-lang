// Expected: E0126 — drop cannot be static.
class Handle { pub fd: int }

impl Handle {
    static fn drop() {}
}

fn main() {}
