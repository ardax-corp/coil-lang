// Enum fn drop() (COI-26): payload variants finalize once; unit variants never.
use gc::collect;

static let drops: int = 0;

static let last: int = 0;

enum Conn {
    Open(int),
    Closed,
}

impl Conn {
    fn drop() {
        drops = drops + 1;
        last = match self {
            Conn::Open(fd) => fd,
            Conn::Closed => -1,
        };
    }
}

fn open_and_forget(int fd) -> int {
    let c = Conn::Open(fd);
    return match c {
        Conn::Open(x) => x,
        Conn::Closed => 0,
    };
}

fn discard(int fd) {
    Conn::Open(fd);
}

test("payload variant drops once when collected") {
    drops = 0;
    open_and_forget(7);
    collect();
    assert(drops == 1)?;
    assert(last == 7)?;
    collect();
    assert(drops == 1)?;
}

test("discarded construction still drops") {
    drops = 0;
    discard(9);
    collect();
    assert(drops == 1)?;
    assert(last == 9)?;
}

test("unit variants never drop") {
    drops = 0;
    let c = Conn::Closed;
    let d = Conn::Closed;
    collect();
    assert(drops == 0)?;
}

test("many values in a loop each drop") {
    drops = 0;
    let i = 0;
    while i < 50 {
        open_and_forget(i);
        i = i + 1;
    }
    collect();
    assert(drops == 50)?;
}

test("explicit drop runs once") {
    drops = 0;
    let c = Conn::Open(3);
    c.drop();
    assert(drops == 1)?;
    c.drop();
    assert(drops == 1)?;
}

enum Shape {
    Rect { w: int, h: int },
    Dot,
}

static let shape_drops: int = 0;

impl Shape {
    fn drop() {
        shape_drops = shape_drops + 1;
    }
}

fn area() -> int {
    let s = Shape::Rect { w: 3, h: 4 };
    return match s {
        Shape::Rect { w, h } => w * h,
        Shape::Dot => 0,
    };
}

test("record payload variant drops") {
    shape_drops = 0;
    assert(area() == 12)?;
    collect();
    assert(shape_drops == 1)?;
}

enum Slot<T> {
    Full(T),
    Empty,
}

static let slot_drops: int = 0;

impl Slot<T> {
    fn drop() {
        slot_drops = slot_drops + 1;
    }
}

fn fill_slot() -> int {
    let s: Slot<int> = Slot::Full(5);
    return match s {
        Slot::Full(v) => v,
        Slot::Empty => 0,
    };
}

test("generic enum drops") {
    slot_drops = 0;
    assert(fill_slot() == 5)?;
    collect();
    assert(slot_drops == 1)?;
}

fn make(int fd) -> Conn {
    return Conn::Open(fd);
}

fn pass_through(Conn c) -> Conn {
    return c;
}

test("escaping value drops once, after it dies") {
    drops = 0;
    let kept = pass_through(make(4));
    collect();
    assert(drops == 0)?;
    let fd = match kept {
        Conn::Open(x) => x,
        Conn::Closed => 0,
    };
    assert(fd == 4)?;
}
