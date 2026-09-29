// Array literals of ground pointer elements carry a pointer element kind
// (`MakeArrayK`): marking treats the elements as precise references and the
// collector may move what they point at. Collections in between must keep
// every element, including `None` holes and nested literals.
use gc::{collect};

class Node {
    pub v: int,
    pub next: Option<Node>,
}

fn node(int v) -> Node {
    return new Node(v, Option::None);
}

test("node literals survive collections") {
    let nodes = [node(1), node(2), node(3), node(4)];
    let junk = 0;
    while junk < 2000 {
        let pair = [node(junk), node(junk + 1)];
        junk = junk + pair[1].v - pair[0].v;
    }
    collect();
    let total = 0;
    let i = 0;
    while i < len(nodes) {
        total = total + nodes[i].v;
        i = i + 1;
    }
    assert(total == 10)?;
}

test("option, nested and string literals") {
    let holes = [Option::Some(node(5)), Option::None, Option::Some(node(7))];
    let rows = [[node(1), node(2)], [node(3), node(4)]];
    let words = ["a", "bb", "ccc"];
    collect();
    let total = 0;
    let i = 0;
    while i < len(holes) {
        total = total + match holes[i] {
            Option::Some(n) => n.v,
            Option::None => 0,
        };
        i = i + 1;
    }
    assert(total == 12)?;
    assert(rows[0][1].v + rows[1][0].v == 5)?;
    assert(len(words[2]) == 3)?;
}
