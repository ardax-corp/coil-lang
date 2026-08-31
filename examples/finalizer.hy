// examples/finalizer.hy — GC-time `fn drop()` on a class (FFI-shaped handle).
//
// Output: closed

use gc::{collect};
use io::{stdout};
use io::sync::{write_all};
use string::{to_bytes};

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
    write_all(stdout(), to_bytes(log));
}
