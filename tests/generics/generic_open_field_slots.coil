// A frame-slot local of a generic class whose field is open in its
// parameter (`Option<Node<T>>`): the field's slot holds the value boxed,
// as the class's heap object would. Built, rebuilt from its own field
// and passed on whole.
use tests::assert;
use gc::collect;

class Node<T> {
    pub value: T,
    pub next: Option<Node<T>>,
}

class Stack<T> {
    pub head: Option<Node<T>>,
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

fn chain(int k) -> int {
    let s: Stack<int> = new Stack(Option::None);
    s.head = Option::Some(new Node(k, s.head));
    collect();
    s.head = Option::Some(new Node(k + 1, s.head));
    return total(s);
}

test("an open niche field in a frame-slot local") {
    assert!(chain(1) == 3);
    assert!(chain(5) == 11);
}
