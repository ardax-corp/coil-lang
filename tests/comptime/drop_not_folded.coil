// A program with a `fn drop()` computes no call or static let at compile
// time: the values they drop must run their finalizers when it runs.
use gc::collect;
use tests::assert;

static let drops = 0;

enum Conn {
    Open(int),
    Closed,
}

impl Conn {
    fn drop() {
        drops = drops + 1;
    }
}

fn open_and_read(int fd) -> int {
    let c = Conn::Open(fd);
    return match c {
        Conn::Open(x) => x,
        Conn::Closed => 0,
    };
}

static let first = open_and_read(3);

fn main() {
    assert!(first == 3);
    assert!(open_and_read(4) == 4);
    collect();
    assert!(drops == 2);
}
