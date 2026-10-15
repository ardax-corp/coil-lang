// Expected: E0102 — a `string` variable does not cast to `[byte; N]`; use `to_bytes`.
fn main() {
    let s = "hi";
    let _ = s as [byte; 2];
}
