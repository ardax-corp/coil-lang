// @expected E0119 — `#[ffi(...)]` is not an attribute; foreign functions are
// declared in an `extern "lib" { fn …; }` block.
#[ffi(lib = "c")]
fn strlen(string s) -> int {
    return 0;
}

fn main() {}
