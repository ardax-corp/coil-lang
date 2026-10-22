// @expected E0101 — `use math::add as plus;` binds only `plus`, so `add` is
// not a function here, though the loaded module defines one (#1211).
use math::add as plus;

fn main() {
    let _ = add(1, 2);
}
