// @expected E0102 — `gc::upgrade` takes a `Weak<T>`; a `Root` is read with `get`.
use gc::{upgrade, root};

fn main() {
    let r = root("x");
    let _ = upgrade(r);
}
