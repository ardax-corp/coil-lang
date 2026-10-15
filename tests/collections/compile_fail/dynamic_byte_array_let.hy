// Expected: E0119
// #890: `[byte]` is rejected like any `[T]`; use `to_bytes(s)` for a `Vec<byte>`.

fn main() {
    let buf: [byte] = "ok";
}
