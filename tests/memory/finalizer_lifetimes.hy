// A local holding an object with `fn drop()` keeps it reachable for as
// long as its frame slot does: a collection never runs the `drop` while
// the local's block is open. Where the compiler proves nothing else holds
// the object, the block's end frees it and runs the `drop` there (after a
// `return`'s value is computed).

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

// Freed where the branch's block ends, before the join.
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

// With early freeing on, the branch's block end runs the `drop` (1);
// without it, the collection after the join may run it (1) or the local's
// slot may still hold it (0). Either way it runs once.
test("a branch's binding is dropped once its block has ended") {
    drops = 0;
    assert!(branchy(true) <= 1);
    collect();
    assert!(drops == 1);
}
