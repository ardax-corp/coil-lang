// Expected: E0102 — wrong argument types (arity alone is currently
// accepted by the checker for some shapes; type mismatch is reliable).
fn add(int a, int b) -> int {
    return a + b;
}

fn main() {
    add("x", "y");
}
