// Expected: E0102 — the copy of `add_first` for a `(string)` pack adds a
// string to an int; the `(int)` copy alone would compile (coil-lang#1281).
fn add_first(... xs) -> int {
    return xs[0] + 1;
}

fn main() {
    let a = add_first(1);
    let b = add_first("s");
}
