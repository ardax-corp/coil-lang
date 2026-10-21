// Output: 11
use gc::collect;
use io::{stdout, write};
use string::{format, to_bytes};

class Handle {
    pub fd: int,
}

static let drops: int = 0;

static let kept: Option<Handle> = Option::None;

impl Handle {
    fn drop() {
        drops = drops + 1;
        kept = Option::Some(self);
    }
}

fn explicit() {
    let h = new Handle(7);
    h.drop();
}

fn clear_kept() {
    kept = Option::None;
}

fn main() {
    explicit();
    collect();
    let d1 = drops;
    clear_kept();
    collect();
    let _ = write(stdout(), to_bytes(format("%i%i", d1, drops)));
}
