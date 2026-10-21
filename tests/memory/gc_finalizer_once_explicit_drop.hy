// Output: 1
use gc::collect;
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
    h.drop();
    h.drop();
    collect();
    let _ = write(stdout(), to_bytes(format("%i", drops)));
}
