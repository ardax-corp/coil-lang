// Expected: E0102 — a non-literal `string` does not cast to `Vec<byte>`;
// use `to_bytes(s)`.
fn main() {
    let s = "hi" + "!";
    let v = s as Vec<byte>;
}
