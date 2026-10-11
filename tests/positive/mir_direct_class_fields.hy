// Class objects in bodies lowered straight from HIR to MIR: class params,
// returns and locals are heap references, and field reads and writes on
// them load and store the object's slot.
class Acc {
    pub n: int,
    pub total: float,
    pub last: int,
}

class Node {
    pub val: int,
    pub next: Acc,
}

class Box<T> {
    pub item: T,
}

fn feed(Acc a, int k) {
    let i = 0;
    while i < k {
        a.n = a.n + 1;
        a.total = a.total + 0.5;
        a.last = i * a.n;
        i = i + 1;
    }
}

fn pick(Acc a, Acc b, bool first) -> Acc {
    if first {
        return a;
    }
    return b;
}

fn relink(Node x, Acc a) -> int {
    let old = x.next.n;
    x.next = a;
    x.next.n = x.next.n + x.val;
    return old;
}

class Link {
    pub val: int,
    pub w: float,
    pub prev: Acc,
}

// Each object is allocated after its arguments; the chain keeps every
// earlier one reachable only through the heap.
fn chain(int n) -> int {
    let head = new Acc(0, 0.0, 0);
    let total = 0;
    let i = 0;
    while i < n {
        let l = new Link(i, 0.25, head);
        head = new Acc(l.val + head.n, l.w + head.total, i);
        total = total + l.prev.n;
        i = i + 1;
    }
    return total + head.n;
}

fn unbox(Box<int> b) -> int {
    b.item = b.item * 3;
    return b.item + 1;
}

test("class fields in direct MIR") {
    let a = new Acc(0, 0.0, 0);
    feed(a, 10);
    assert(a.n == 10)?;
    assert(a.total == 5.0)?;
    assert(a.last == 90)?;
    let b = new Acc(7, 1.0, 0);
    let p = pick(a, b, false);
    p.n = p.n + 1;
    assert(b.n == 8)?;
    assert(pick(a, b, true).n == 10)?;
    let x = new Node(4, b);
    assert(relink(x, a) == 8)?;
    assert(a.n == 14)?;
    assert(b.n == 8)?;
    assert(unbox(new Box(5)) == 16)?;
}

test("objects allocated in direct MIR") {
    assert(chain(100) == 166650)?;
}
