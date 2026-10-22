// @output 0
use gc::{collect, root};
use io::{stdout, write};
use string::{format, to_bytes};

static let drops: int = 0;

class Handle {
    pub fd: int,
}

impl Handle {
    fn drop() {
        drops = drops + 1;
    }
}

fn main() {
    let h = new Handle(1);
    let r = root(h);
    collect();
    let _ = write(stdout(), to_bytes(format("%i", drops)));
}
