// Expected: compile failure — fn drop must return unit (E0126).
class Handle { pub fd: int }

impl Handle {
    fn drop() -> int {
        return 0;
    }
}

fn main() {}
