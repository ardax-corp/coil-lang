// Expected: E0001 — `pub` is not allowed on a top-level `static let`; every top-level item is importable without it.
pub static let counter = 0;

fn main() {
    println(counter);
}
