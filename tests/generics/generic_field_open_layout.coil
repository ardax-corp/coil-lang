// A generic class's slot holds its field at the open type's layout: a
// `Box<T>`'s `item` is a boxed word whatever `T` is. Read at a ground
// receiver whose type has a one-word layout of its own (a niche
// `Option<string>`, `Option<Node<T>>`), the word converts from the boxed
// layout to the read's (#976).
use tests::assert;

class Box<T> {
    pub item: T,
}

class Node<T> {
    pub value: T,
    pub next: Option<Node<T>>,
}

class Stack<T> {
    pub head: Option<Node<T>>,
}

fn name(Box<Option<string>> b) -> string {
    return b.item ?? "none";
}

fn total(Stack<int> s) -> int {
    let n = 0;
    let at = s.head;
    while let Option::Some(node) = at {
        n = n + node.value;
        at = node.next;
    }
    return n;
}

test("niche option field of a generic class") {
    assert!(name(new Box(Option::Some("ann"))) == "ann");
    assert!(name(new Box(Option::None)) == "none");
}

test("generic node chain") {
    let s: Stack<int> = new Stack(Option::None);
    s.head = Option::Some(new Node(1, s.head));
    s.head = Option::Some(new Node(2, s.head));
    assert!(total(s) == 3);
}
