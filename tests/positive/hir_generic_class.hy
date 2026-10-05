class Stack<T> {
    items: Vec<T>,
    n: int,
}

impl Stack<T> {
    pub static fn new() -> Stack<T> {
        let items: Vec<T> = Vec::new();
        return new Stack(items, 0);
    }

    pub fn push(T v) {
        if self.n < len(self.items) {
            self.items[self.n] = v;
        } else {
            self.items.push(v);
        }
        self.n = self.n + 1;
    }

    pub fn size() -> int {
        return self.n;
    }

    pub fn pop() -> Option<T> {
        if self.n == 0 {
            return Option::None;
        }
        self.n = self.n - 1;
        return Option::Some(self.items[self.n]);
    }
}

fn drain(Stack<int> s) -> int {
    let total = 0;
    while s.size() > 0 {
        match s.pop() {
            Option::Some(x) => {
                total = total * 10 + x;
            },
            Option::None => {},
        }
    }
    return total;
}

fn names() -> Stack<string> {
    let s = Stack::new();
    s.push("a");
    s.push("bc");
    return s;
}

test("generic class locals and methods lower") {
    let s = Stack::new();
    s.push(3);
    s.push(4);
    assert(drain(s) == 43)?;
    let t = names();
    assert(t.size() == 2)?;
    match t.pop() {
        Option::Some(top) => assert(top == "bc")?,
        Option::None => assert(false)?,
    }
    assert(t.size() == 1)?;
}
