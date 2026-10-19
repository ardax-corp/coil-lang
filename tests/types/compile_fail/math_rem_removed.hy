// Expected: E0101 — `prelude::math::rem` was removed; write `x % y`.
use prelude::math::{rem};

fn main() {
    let _ = rem(7.5, 2.0);
}
