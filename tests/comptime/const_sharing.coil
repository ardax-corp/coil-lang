// A const's value is one object, made once while compiling: every read
// sees the same object, and one const built from another shares it.
use tests::assert;

class Node {
    pub value: int,
    pub next: Option<Node>,
}

fn chain(int n) -> Node {
    let head = new Node(0, None);
    let i = 1;
    while i < n {
        head = new Node(i, Some(head));
        i = i + 1;
    }
    return head;
}

fn count(Option<Node> n) -> int {
    return match n {
        Some(node) => 1 + count(node.next),
        None => 0,
    };
}

class Wrapper {
    pub inner: Node,
}

fn wrap(Node n) -> Wrapper {
    return new Wrapper(n);
}

const LIST = chain(500);

const WRAPPED = wrap(LIST);

test("a long linked structure comes back whole") {
    assert!(LIST.value == 499);
    assert!(count(Some(LIST)) == 500);
}

test("a const built from another shares its object") {
    LIST.value = 1000;
    assert!(WRAPPED.inner.value == 1000);
}
