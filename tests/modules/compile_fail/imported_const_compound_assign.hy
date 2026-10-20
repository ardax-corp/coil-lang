// Expected: E0107 — `+=` on an imported `const` is a reassignment too
// (#1210).
use mod_state::GREETING;

fn main() {
    GREETING += "!";
}
