// @output 1
use gc::collect;
use io::{stdout, write};
use string::{format, to_bytes};

static let drops: int = 0;

class Handle {
    pub fd: int,
}

impl Handle {
    fn drop() {
        collect();
        drops = drops + 1;
    }
}

fn make() {
    let h = new Handle(1);
}

fn main() {
    make();
    collect();
    let _ = write(stdout(), to_bytes(format("%i", drops)));
}
