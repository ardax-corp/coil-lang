// Typed inlining splices callees with heap locals (strings, classes,
// options) and clears their slots after the splicing statement. A callee
// that builds an object with a finalizer keeps its call, so `drop` still
// runs at the next collection.

use gc::collect;
use string::format;

class Node {
    pub v: int,
    pub next: Option<Node>,
}

class Handle {
    pub fd: int,
}

static let closed: int = 0;

impl Handle {
    fn drop() {
        closed = closed + 1;
    }
}

fn label(int n) -> string {
    let s = format("n=%i", n);
    return s;
}

fn pair_sum(int a, int b) -> int {
    let m = new Node(b, Option::None);
    let n = new Node(a, Option::Some(m));
    return n.v + m.v;
}

fn leak(int fd) {
    let h = new Handle(fd);
}

// Test bodies do not inline; these callers do.
fn sums() -> int {
    let total = 0;
    let i = 0;
    while i < 5 {
        total = total + pair_sum(i, 10);
        i = i + 1;
    }
    return total;
}

// A loop the compiler may split across workers: the spliced `new`s are
// emitted in the worker's frame.
fn sums_to(int k) -> int {
    let acc = 0;
    let i = 0;
    while i < k {
        acc = acc + pair_sum(i, 10);
        i = i + 1;
    }
    return acc;
}

fn labelled() -> string {
    let s = label(7);
    return s;
}

fn leak_and_collect() -> int {
    leak(3);
    collect();
    return closed;
}

test("heap locals splice and the result is right") {
    assert(sums() == 60)?;
    assert(sums_to(1000) == 509500)?;
    assert(labelled() == "n=7")?;
}

test("a finalized object still drops at the next collection") {
    assert(leak_and_collect() == 1)?;
}
