// Helper for tests/modules/module_resolution.hy: a non-entry module that
// `use`s a sibling module's free function.
use ns_protocol::payload_eq;

fn check() -> int {
    return payload_eq(7, 7);
}
