// Helper for tests/modules/module_resolution.hy: free functions a sibling
// dependency (`ns_server`) imports.

fn payload_eq(int a, int b) -> int {
    if a == b {
        return 1;
    }
    return 0;
}
