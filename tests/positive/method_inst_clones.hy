// A generic class's method, or an inherent method with its own type
// parameters, called at ground word types gets a per-type clone
// (`Stack::push$inst$int`) that takes the receiver's raw words; the
// shared body stays for the other calls. Arguments and results that do not
// fit a clone (pairs, open types) still reach the shared body.
use string::format;

class Node<T> {
    pub value: T,
    pub next: Option<Node<T>>,
}

class Stack<T> {
    pub head: Option<Node<T>>,
    pub size: int,
}

impl Stack<T> {
    pub static fn empty() -> Stack<T> {
        return new Stack(Option::None, 0);
    }

    pub fn push(T value) {
        self.head = Option::Some(new Node(value, self.head));
        self.size = self.size + 1;
    }

    pub fn pop() -> Option<T> {
        return match self.head {
            Option::Some(node) => {
                self.head = node.next;
                self.size = self.size - 1;
                Option::Some(node.value)
            },
            Option::None => Option::None,
        };
    }

    pub fn top() -> T {
        return match self.head {
            Option::Some(node) => node.value,
            Option::None => panic "empty stack",
        };
    }

    pub fn both(T a) -> (T, T) {
        return (a, self.top());
    }
}

trait Weigh<T> {
    fn weigh(T x) -> int {}
}

impl Weigh for int {
    pub fn weigh(int x) -> int {
        return x * 2;
    }
}

impl Weigh for string {
    pub fn weigh(string x) -> int {
        return x.len();
    }
}

class Scale {
    pub bias: int,
}

impl Scale {
    pub fn read<T: Weigh>(T x) -> int {
        return x.weigh() + self.bias;
    }

    pub static fn of<T: Weigh>(T x) -> int {
        return x.weigh();
    }

    pub fn pick<T>(bool first, T a, T b) -> T {
        if first {
            return a;
        }
        return b;
    }
}

fn sum_ints(int n) -> int {
    let s: Stack<int> = Stack::empty();
    let i = 0;
    while i < n {
        s.push(i);
        i = i + 1;
    }
    let total = 0;
    while let Option::Some(v) = s.pop() {
        total = total + v;
    }
    return total;
}

test("generic class methods at int") {
    assert(sum_ints(100) == 4950)?;
    let s: Stack<int> = Stack::empty();
    s.push(7);
    s.push(9);
    assert(s.top() == 9 && s.size == 2)?;
    assert(s.both(1) == (1, 9))?;
}

test("generic class methods at string survive allocation") {
    let s: Stack<string> = Stack::empty();
    let i = 0;
    while i < 300 {
        s.push(format("s%v", i));
        i = i + 1;
    }
    assert(s.top() == "s299")?;
    assert((s.pop() ?? "") == "s299")?;
    assert(s.size == 299)?;
}

test("generic class methods at float and bool") {
    let f: Stack<float> = Stack::empty();
    f.push(1.5);
    assert(f.top() == 1.5)?;
    let b: Stack<bool> = Stack::empty();
    assert(b.pop() == Option::None)?;
    b.push(true);
    assert(b.pop() == Option::Some(true))?;
}

test("generic methods of a plain class") {
    let sc = new Scale(1);
    assert(sc.read(4) == 9)?;
    assert(sc.read("abc") == 4)?;
    assert(Scale::of(5) == 10)?;
    assert(sc.pick(false, "a", "b") == "b")?;
    assert(sc.pick(true, 3, 4) == 3)?;
}
