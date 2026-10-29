// @expected E0101 — `prelude::math::sqrt` is coil-stdlib's `math::sqrt` now.
use prelude::math::sqrt;

fn main() {
    let _ = sqrt(2.0);
}
