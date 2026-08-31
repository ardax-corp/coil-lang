// Expected: compile failure — at most one fn drop per class (E0126).
class Handle { pub fd: int }

impl Handle {
    fn drop() {}
    fn drop() {}
}

fn main() {}
