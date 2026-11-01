// @expected E0102 — `gc::get` takes a `Root<T>`; a `Weak<T>` goes through `upgrade`.
use gc::{get, weak};

fn main() {
    let w = weak(1);
    let _ = get(w);
}
