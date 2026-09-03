// Typed class fields use slot LoadField/SetField; checksum matches gc_churn.
class Node {
    pub v: int,
    pub next: Option<Node>,
}

fn build_list(int n) -> Option<Node> {
    let head: Option<Node> = Option::None;
    let i = n - 1;
    while i >= 0 {
        let node = new Node(i, Option::None);
        node.next = head;
        head = Option::Some(node);
        i = i - 1;
    }
    return head;
}

fn checksum_walk(Option<Node> cur) -> int {
    let acc = 0;
    let go = true;
    while go {
        acc = acc + match cur {
            Option::Some(node) => {
                cur = node.next;
                node.v
            },
            Option::None => {
                go = false;
                0
            },
        };
    }
    return acc;
}

test("typed slots get/set declaration order") {
    let p = new Node(10, Option::None);
    assert(p.v == 10)?;
    p.v = 21;
    assert(p.v == 21)?;
    let nxt = new Node(3, Option::None);
    p.next = Option::Some(nxt);
    assert(match p.next {
        Option::Some(n) => n.v,
        Option::None => -1,
    } == 3)?;
}

test("gc_churn shape checksum") {
    let n = 20;
    let rounds = 3;
    let acc = 0;
    let r = 0;
    while r < rounds {
        acc = acc + checksum_walk(build_list(n));
        r = r + 1;
    }
    // 3 * 20 * 19 / 2
    assert(acc == 570)?;
}

