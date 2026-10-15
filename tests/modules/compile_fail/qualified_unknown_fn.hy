// Expected: E0101 — `math` has no function `nope`.
use math::add;

fn main() {
    let _ = math::nope(1);
}
