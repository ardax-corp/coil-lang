// @output closed
// A finalizer for garbage still uncollected when `main` returns runs at
// teardown, and the harness sees its output (#1227).
use io::{stdout, write};
use string::to_bytes;

class Handle {
    pub fd: int,
}

impl Handle {
    fn drop() {
        let _ = write(stdout(), to_bytes("closed"));
    }
}

fn make() {
    let h = new Handle(1);
}

fn main() {
    make();
}
