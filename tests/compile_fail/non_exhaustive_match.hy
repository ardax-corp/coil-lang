// Expected: E0209 — non-exhaustive match.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
enum Color {
    Red,
    Green,
    Blue,
}

fn main() {
    let c = Color::Red;
    write_all(stdout(), to_bytes(format("%i", match c {
        Color::Red => 0,
    })));
}
