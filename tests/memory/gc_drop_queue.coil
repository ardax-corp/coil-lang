// Finalizers: unreachable objects that reference each other are all
// finalized by one collection, each once; a panic in one `drop` stops only
// that finalizer; a generic class's `drop` runs too.
use tests::assert;
use gc::collect;

static let drops: int = 0;

static let ok_drops: int = 0;

class Node {
    pub next: Option<Node>,
}

impl Node {
    fn drop() {
        drops += 1;
    }
}

class Bad {
    pub n: int,
}

impl Bad {
    fn drop() {
        if self.n > 0 {
            panic "bad drop";
        }
        ok_drops += 1;
    }
}

class Holder<T> {
    pub value: T,
}

impl Holder<T> {
    fn drop() {
        drops += 10;
    }
}

fn chain(int n) {
    let head = new Node(Option::None);
    let i = 1;
    while i < n {
        head = new Node(Option::Some(head));
        i += 1;
    }
}

fn cycle() {
    let a = new Node(Option::None);
    let b = new Node(Option::Some(a));
    a.next = Option::Some(b);
}

fn some_bad() {
    let _a = new Bad(1);
    let _b = new Bad(0);
    let _c = new Bad(2);
    let _d = new Bad(0);
}

fn holders() {
    let _a = new Holder(1);
    let _b = new Holder("s");
}

test("a chain of unreachable nodes is finalized by one collect") {
    drops = 0;
    chain(5);
    collect();
    assert!(drops == 5);
    collect();
    assert!(drops == 5);
}

test("an unreachable cycle is finalized, each node once") {
    drops = 0;
    cycle();
    collect();
    assert!(drops == 2);
    collect();
    assert!(drops == 2);
}

test("a panicking drop does not stop the others") {
    ok_drops = 0;
    some_bad();
    collect();
    assert!(ok_drops == 2);
}

test("a generic class's drop runs for each instantiation") {
    drops = 0;
    holders();
    collect();
    assert!(drops == 20);
}
