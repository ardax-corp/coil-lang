// Expected: E0107 — `+=` on an imported `static const` is a reassignment too
// (#1210).
use mod_state::GREETING;

fn main() {
    GREETING += "!";
}
