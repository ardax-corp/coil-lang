// Helper for tests/modules/module_resolution.hy: a one-item file in a
// directory, reached as `ns_util::inc`.

fn inc(int x) -> int {
    return x + 1;
}
