// Expected: E0126 — at most one fn drop per class.
class Handle { pub fd: int }

impl Handle {
    fn drop() {}
    fn drop() {}
}

fn main() {}
