// @expected E0102 — the `declare`d signature takes an `Int`; a `Stream` is not one.
use ffi::declare;
use ffi::invoke;
use ffi::types::Int;
use io::stdout;

fn main() {
    let lib = 7;
    let id = match declare(lib, "labs", (Int,), Int) {
        Result::Ok(id) => id,
        Result::Err(_) => 0,
    };
    let s = stdout();
    let _ = invoke(lib, id, (s,));
}
