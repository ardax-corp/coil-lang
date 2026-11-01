// @output ok\n
// The FFI `ptr` type names a type outside `extern` blocks too, so the
// unknown-type check (#1961) must not report it.
use io::sync::println;

fn keep(ptr p) -> ptr {
    return p;
}

fn main() {
    let _ = println("ok");
}
