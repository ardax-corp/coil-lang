// Helper for tests/modules/module_resolution.hy: a `same` for strings that
// `ns_ints`' own later `same` must not resolve to.

fn same(string a, string b) -> bool {
    return a == b;
}
