// `Vec<P>` built by `Vec::new` / `with_capacity` / `from` with a ground heap
// element type carries a pointer element kind (`TagArrayKind`): marking
// treats the elements as precise references. Collections in between must
// keep every element (and `None` / `0` holes) intact.
use gc::collect;

class Node {
    pub v: int,
    pub next: Option<Node>,
}

fn chain(int n) -> Vec<Node> {
    let out: Vec<Node> = Vec::with_capacity(n);
    let prev: Option<Node> = Option::None;
    let i = 0;
    while i < n {
        let node = new Node(i, prev);
        out.push(node);
        prev = Option::Some(node);
        i = i + 1;
    }
    return out;
}

fn sum(Vec<Node> nodes) -> int {
    let total = 0;
    let i = 0;
    while i < len(nodes) {
        total = total + nodes[i].v;
        i = i + 1;
    }
    return total;
}

test("pointer vec survives collections") {
    let nodes = chain(500);
    collect();
    let more = chain(300);
    collect();
    assert(sum(nodes) == 124750)?;
    assert(sum(more) == 44850)?;
    let tail = match nodes[499].next {
        Option::Some(n) => n.v,
        Option::None => -1,
    };
    assert(tail == 498)?;
}

test("vec of options keeps holes and payloads") {
    let slots: Vec<Option<Node>> = Vec::new();
    let i = 0;
    while i < 100 {
        if i % 3 == 0 {
            slots.push(Option::None);
        } else {
            slots.push(Option::Some(new Node(i, Option::None)));
        }
        i = i + 1;
    }
    collect();
    let total = 0;
    let j = 0;
    while j < len(slots) {
        total = total + match slots[j] {
            Option::Some(n) => n.v,
            Option::None => 0,
        };
        j = j + 1;
    }
    assert(total == 3267)?;
}

test("nested and literal-backed vecs") {
    let rows: Vec<Vec<Node>> = Vec::new();
    let r = 0;
    while r < 10 {
        rows.push(chain(r + 1));
        r = r + 1;
    }
    let words: Vec<string> = Vec::from(["a", "bb", "ccc"]);
    collect();
    let total = 0;
    let k = 0;
    while k < len(rows) {
        total = total + len(rows[k]);
        k = k + 1;
    }
    assert(total == 55)?;
    assert(len(words[2]) == 3)?;
}
