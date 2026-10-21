// Expected: E0101 — `prelude::math::pow` was removed; write `base ** exponent`.
use prelude::math::pow;

fn main() {
    let _ = pow(2.0, 10.0);
}
