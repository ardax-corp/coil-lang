// Expected: E0413 — the built-in `Show` declares no effects yet, so `show(x)` on an
// existential is not allowed in a `pure fn`.
pure fn text(Show x) -> string {
    return show(x);
}

fn main() {
    text(1);
}
