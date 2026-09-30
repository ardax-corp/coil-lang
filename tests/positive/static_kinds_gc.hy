// Statics carry compile-time word kinds (archive minor 29): a pointer static
// is a precise root the collector may rewrite when its target moves, a scalar
// static is no root. Collections (and evacuation) in between must keep every
// static's value intact.
use gc::collect;

class Node {
    pub v: int,
    pub next: Option<Node>,
}

static let head: Option<Node> = Option::None;

static let nodes: Vec<Node> = Vec::new();

static let count: int = 0;

static let label: string = "static";

fn fill(int n) {
    let i = 0;
    while i < n {
        let node = new Node(i, head);
        head = Option::Some(node);
        nodes.push(node);
        count = count + 1;
        i = i + 1;
    }
}

test("static roots survive collections") {
    fill(500);
    let junk = 0;
    while junk < 2000 {
        let t = new Node(junk, Option::None);
        junk = junk + t.v - t.v + 1;
    }
    collect();
    fill(10);
    collect();
    let first = match head {
        Option::Some(n) => n.v,
        Option::None => -1,
    };
    assert(first == 9)?;
    assert(len(nodes) == 510)?;
    assert(nodes[499].v == 499)?;
    assert(count == 510)?;
    assert(len(label) == 6)?;
}
