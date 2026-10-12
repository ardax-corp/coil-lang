// A generic class's shared method stores its type parameter's value into a
// new object; the value must survive the allocations before the store.
use string::format;

class Node<T> {
    pub value: T,
    pub next: Option<Node<T>>,
}

class Stack<T> {
    head: Option<Node<T>>,
    pub len: int,
}

impl Stack<T> {
    pub static fn new() -> Stack<T> {
        return new Stack(Option::None, 0);
    }

    pub fn push(T v) {
        self.head = Option::Some(new Node(v, self.head));
        self.len = self.len + 1;
    }

    pub fn top() -> Option<T> {
        return match self.head {
            Option::None => Option::None,
            Option::Some(n) => Option::Some(n.value),
        };
    }
}

fn fill(int n) -> Stack<string> {
    let names: Vec<string> = Vec::new();
    names.push("a");
    names.push("b");
    names.push("c");
    let s: Stack<string> = Stack::new();
    let i = 0;
    while i < n {
        s.push(format("item %s", names[i % 3]));
        i = i + 1;
    }
    return s;
}

test("strings pushed through a shared method") {
    let s = fill(50);
    assert(s.len == 50)?;
    assert((s.top() ?? "none") == "item b")?;
}

test("ints pushed through a shared method") {
    let s = Stack::new();
    s.push(3);
    s.push(8);
    assert(s.len == 2)?;
    assert((s.top() ?? 0) == 8)?;
}
