// Expected: compile failure — calling an FFI process-exec symbol requires `--allow-ffi-exec`.
extern "plugin" {
    fn system() -> int;
}

fn main() {
    let _ = system();
}
