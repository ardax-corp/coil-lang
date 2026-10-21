// Expected: E0101 — `io::to_bytes` was removed; it is `string::to_bytes`.
use io::to_bytes;

fn main() {
    let _ = to_bytes("hi");
}
