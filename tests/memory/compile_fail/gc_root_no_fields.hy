// Expected: E0119 — a `Root<T>` is opaque; read the value with `gc::get`.
use gc::{root};

fn main() {
    let r = root(1);
    let _ = r.n;
}
