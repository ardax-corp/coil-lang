// Expected: E0801 — a module-level `const` must fold at compile time;
// `static const` is the form for a value computed at startup.
fn four() -> int {
    return 4;
}

const D = four() + 1;

fn main() {
    let _ = D;
}
