// Expected: E0119
// #890: `s as [byte]` is gone; `to_bytes(s)` returns `Vec<byte>`.

fn main() {
    let s = "hi";
    let b = s as [byte];
}
