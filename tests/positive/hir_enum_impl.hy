// A user enum with methods and a drop lowers like any other enum: built,
// matched, passed and returned.
use gc::collect;

static let drops: int = 0;

enum Conn {
    Open(int),
    Closed,
}

impl Conn {
    pub fn fd() -> int {
        return match self {
            Conn::Open(fd) => fd,
            Conn::Closed => -1,
        };
    }

    fn drop() {
        drops = drops + 1;
    }
}

fn connect(int fd) -> Conn {
    return Conn::Open(fd);
}

fn consume(int fd) -> int {
    let c = connect(fd);
    return match c {
        Conn::Open(x) => x + 1,
        Conn::Closed => 0,
    };
}

fn closed() -> int {
    let c = Conn::Closed;
    return c.fd();
}

test("enum with methods") {
    assert(consume(4) == 5)?;
    assert(closed() == -1)?;
    assert(connect(9).fd() == 9)?;
}

test("payload variant still drops") {
    drops = 0;
    consume(7);
    collect();
    assert(drops >= 1)?;
}
