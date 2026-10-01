// A libc binding for tests/compile_fail/extern_libc_in_module_denied.hy:
// `extern "c"` is always denied, so any program using this module fails.
extern "c" {
    fn strlen(string s) -> int;
}

fn c_strlen(string s) -> int {
    return strlen(s);
}
