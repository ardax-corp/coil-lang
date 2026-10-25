// @output ok\n
// The FFI type names (`ptr`, `int32`, ...) name types outside `extern`
// blocks too, so the unknown-type check (#1961) must not report them.
use io::sync::println;

fn keep(ptr p) -> ptr {
    return p;
}

fn narrow(int32 x, uint8 y) -> int {
    return 0;
}

fn main() {
    let _ = println("ok");
}
