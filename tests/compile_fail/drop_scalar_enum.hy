// Expected: compile failure — scalar-backed enums are not heap values (E0126).
#[repr(int)]
enum Level {
    Low = 1,
    High = 2,
}

impl Level {
    fn drop() {}
}

fn main() {}
