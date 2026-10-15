// Helper for tests/modules/module_resolution.hy: the contract clause and body
// name this module's later `same`, not `ns_strs::same`.

fn pick(int x) -> int
    ensures same(result, x)
{
    if same(x, x) {
        return x;
    }
    return 0;
}

fn same(int a, int b) -> bool {
    return a == b;
}
