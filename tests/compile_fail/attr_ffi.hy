// Expected: compile failure — compile-time FFI is `extern "lib" { fn …; }`.
#[ffi(lib = "c")]
fn strlen(string s) -> int {
    return 0;
}

fn main() {}
