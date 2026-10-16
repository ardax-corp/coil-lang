// Expected: E0107 — an imported `static const` keeps its rule: it cannot be
// reassigned, as one of the same file cannot (#1210).
use mod_state::GREETING;

fn main() {
    GREETING = "bye";
}
