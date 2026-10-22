// @expected E0102 — `gc::get` takes a `Root<T>`, not a plain value.
use gc::get;

fn main() {
    let _ = get(5);
}
