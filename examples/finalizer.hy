// examples/finalizer.hy — GC-time `fn drop()` on a class (FFI-shaped handle).
//
// @output closed

use gc::collect;
use io::sync::print;

static let log: string = "";

class Handle {
    pub fd: int,
}

impl Handle {
    fn drop() {
        log = "closed";
    }
}

fn leak_handle() {
    let h = new Handle(3);
}

fn main() {
    leak_handle();
    collect();
    let _ = print(log);
}
