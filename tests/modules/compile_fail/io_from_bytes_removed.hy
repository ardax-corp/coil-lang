// @expected E0101 — `io::from_bytes` was removed; it is `string::from_bytes`.
use io::{stdout, from_bytes};

fn main() {
    let _ = from_bytes(Vec::from([104 as byte]));
}
