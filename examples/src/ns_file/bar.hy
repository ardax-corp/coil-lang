// Helper for tests/modules/module_resolution.hy: `use ns_file::…` reads
// `ns_file.hy` and must not reach this file's namesake.

fn top_only() -> string {
    return "BAD";
}
