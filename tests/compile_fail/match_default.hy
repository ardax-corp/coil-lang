// Expected: parse failure — match wildcard is `_` only.
fn main() {
    match 1 {
        default => 0,
    };
}
