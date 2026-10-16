// Helper for tests/modules/module_resolution.hy: the only file of the
// `ns_dir` directory (there is no `ns_dir.hy`), reached as `ns_dir::item`.
use string::format;

fn item() -> string {
    return format("%x\n", 420);
}
