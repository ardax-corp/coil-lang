// Helper for tests/functions/const_fn_value_imported.hy: a `const` and a
// `static` holding function values, imported by their bare names.

fn twice(int x) -> int {
    return x * 2;
}

const TWICE = twice;

static let HOOK: fn(int) -> int = twice;
