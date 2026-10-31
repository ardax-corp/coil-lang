// Bounded generic functions (`Ord`, `Num`, `Show`), a generic stack class, a generic enum and a two-parameter derived class.
// @output 3 2.5 b 3 1 2 10 [1, 2] <5> true\n
use io::sync::println;
use string::format;

fn largest<T: Ord>(T a, T b) -> T {
    if a > b {
        return a;
    }
    return b;
}

fn twice<T: Num>(T x) -> T {
    return x + x;
}

fn describe<T: Show>(T x) -> string {
    return format("<%v>", x);
}

class Stack<T> {
    items: Vec<T>,
}

impl Stack<T> {
    pub static fn new() -> Stack<T> {
        return new Stack(Vec::new());
    }
    pub fn push(T x) {
        self.items.push(x);
    }
    pub fn pop() -> Option<T> {
        return self.items.pop();
    }
    pub fn len() -> int {
        return self.items.len();
    }
}

enum Box<T> {
    Empty,
    Full(T),
}

fn unbox<T>(Box<T> b, T dflt) -> T {
    return match b {
        Box::Empty => dflt,
        Box::Full(v) => v,
    };
}

#[derive(Show, Eq)]
class Pair<A, B> {
    pub a: A,
    pub b: B,
}

fn main() {
    let s: Stack<int> = Stack::new();
    s.push(1);
    s.push(2);
    let p = s.pop() ?? 0;
    let ss: Stack<string> = Stack::new();
    ss.push("x");
    let pr = new Pair(1, "one");
    let pr2 = new Pair(1, "one");
    let _ = println(
        format(
            "%i %f %s %i %i %i %i %v %s %z",
            largest(3, 1),
            twice(1.25),
            largest("a", "b"),
            unbox(Box::Full(3), 0),
            unbox(Box::Empty, 1),
            p,
            twice(5),
            [1, 2],
            describe(5),
            pr == pr2,
        ),
    );
    let _ = (s.len(), ss.len(), pr.a, pr.b);
}
