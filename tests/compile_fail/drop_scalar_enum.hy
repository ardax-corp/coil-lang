// Expected: E0126 — scalar-backed enums are not heap values.
#[repr(int)]
enum Level {
    Low = 1,
    High = 2,
}

impl Level {
    fn drop() {}
}

fn main() {}
