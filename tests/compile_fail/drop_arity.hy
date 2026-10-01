// Expected: E0126 — drop takes no extra parameters.
class Handle { pub fd: int }

impl Handle {
    fn drop(int x) {}
}

fn main() {}
