class Node {
    pub v: int,
    pub next: Option<Node>,
}

fn take_node(Option<Node> cur) -> int {
    return match cur {
        Option::Some(node) => node.v,
        Option::None => 0,
    };
}

fn take_int(Option<int> o) -> int {
    return match o {
        Option::Some(n) => n,
        Option::None => -1,
    };
}

test("option of class niches: Some and None") {
    let leaf = new Node(7, Option::None);
    assert(take_node(Option::Some(leaf)) == 7)?;
    assert(take_node(Option::None) == 0)?;
}

test("option of class match and coalesce") {
    let leaf = new Node(3, Option::None);
    let some = Option::Some(leaf);
    let none: Option<Node> = Option::None;
    assert((some ?? new Node(0, Option::None)).v == 3)?;
    assert((none ?? new Node(9, Option::None)).v == 9)?;
}

test("option of class if on match result") {
    let leaf = new Node(1, Option::None);
    let n = take_node(Option::Some(leaf));
    if n == 1 {
        assert(true)?;
    } else {
        assert(false)?;
    }
}

test("option int stays boxed and matches") {
    assert(take_int(Option::Some(41)) == 41)?;
    assert(take_int(Option::None) == -1)?;
}
