// A local holding an object with `fn drop()` keeps it reachable for as
// long as its frame slot does: until the local is rebound, or the function
// returns. Bodies lowered straight to MIR keep those words live to the
// same points, so a collection never runs a `drop` earlier than under
// stack lowering.

use tests::assert;
use gc::collect;

static let drops: int = 0;

class Handle {
    pub fd: int,
}

impl Handle {
    fn drop() {
        drops = drops + 1;
    }
}

fn peek(Handle h) -> int {
    return h.fd;
}

// Never read again, yet held to the return.
fn unread() -> int {
    let h = new Handle(1);
    collect();
    return drops;
}

// Read once, then held across the collection.
fn read_then_collect() -> int {
    let h = new Handle(2);
    let fd = peek(h);
    collect();
    return drops * 10 + fd;
}

// Held on the path that bound it, across the join.
fn branchy(bool make) -> int {
    if make {
        let h = new Handle(5);
    }
    collect();
    return drops;
}

test("an unread local is held to the return") {
    drops = 0;
    assert!(unread() == 0);
    collect();
    assert!(drops == 1);
}

test("a read local is held past its last read") {
    drops = 0;
    assert!(read_then_collect() == 2);
    collect();
    assert!(drops == 1);
}

test("a branch's binding is held past the join") {
    drops = 0;
    assert!(branchy(true) == 0);
    collect();
    assert!(drops == 1);
}
