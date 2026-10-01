// Expected: E0126 — fn drop must return unit.
class Handle { pub fd: int }

impl Handle {
    fn drop() -> int {
        return 0;
    }
}

fn main() {}
