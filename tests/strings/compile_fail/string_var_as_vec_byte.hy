// Expected: E0102 — non-literal string as Vec<byte> (use to_bytes).
fn main() {
    let s = "hi";
    let _ = s as Vec<byte>;
}
