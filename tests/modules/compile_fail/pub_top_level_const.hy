// Expected: E0001 — `pub` is not allowed on a top-level `const`; every top-level item is importable without it.
pub const LIMIT = 10;

fn main() {
    println(LIMIT);
}
