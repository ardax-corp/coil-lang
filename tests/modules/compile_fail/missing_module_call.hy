// Expected: E0900 — a call to an import from a missing module reports the
// missing module (and not an internal lowering error).
use nothere::x;

fn main() {
    x();
}
