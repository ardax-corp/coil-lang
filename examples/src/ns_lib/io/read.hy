// Helper for tests/modules/module_resolution.hy: a module two directories
// deep, reached as `ns_lib::io::read`.
use string::format;

fn read() -> string {
    return format("%i", 7);
}
