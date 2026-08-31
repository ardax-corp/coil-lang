// Expected: compile failure — drop takes no extra parameters (E0126).
class Handle { pub fd: int }

impl Handle {
    fn drop(int x) {}
}

fn main() {}
