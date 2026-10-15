// Helper for tests/modules/module_resolution.hy: imports `task`, whose own
// `channel` must not replace an importer's `thread::channel`.
use task::{scope, Scope};

fn one() -> int {
    return 1;
}
