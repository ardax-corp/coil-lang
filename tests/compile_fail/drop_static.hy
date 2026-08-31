// Expected: compile failure — drop cannot be static (E0126).
class Handle { pub fd: int }

impl Handle {
    static fn drop() {}
}

fn main() {}
