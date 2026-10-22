// @expected E0410 — an `extern` block naming a library no `--allow-dload`
// grants is denied at compile time.
extern "this_library_definitely_does_not_exist_xyzzy" {
    fn noop() -> int;
}

fn main() {
    let _ = noop();
}
