// Helper for tests/modules/module_resolution.hy: a module file with no
// directory items of its own (`ns_file/` holds an unrelated module).

fn top_only() -> string {
    return "ok";
}

fn add(int a, int b) -> int {
    return a + b;
}

fn helper() -> int {
    return 7;
}

// Calls a sibling by its bare name.
fn public_fn() -> int {
    return helper();
}
