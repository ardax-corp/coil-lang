// Helper for tests/functions/qualified_fn_value_call.hy: a `const`, a
// `static` and a class static field holding function values, called
// through their module path (#1705).

fn twice(int x) -> int {
    return x * 2;
}

fn add(int a, int b) -> int {
    return a + b;
}

const TWICE = twice;

const ADD = add;

static let HOOK: fn(int) -> int = twice;

class Ops {
    pub static triple: fn(int) -> int = fn (int x) => x * 3,
}
