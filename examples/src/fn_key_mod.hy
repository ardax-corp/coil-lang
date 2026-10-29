// Helper module for tests/modules/fn_key_across_modules.hy: functions named
// like the test's own, so `symbol::fn_key()` must tell modules apart.
use symbol::fn_key;

fn plain(int x) -> int {
    return fn_key();
}

fn keyed() -> int {
    return fn_key();
}
